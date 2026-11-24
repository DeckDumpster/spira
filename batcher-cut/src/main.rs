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

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use batcher::drive;
use batcher_cut::judge::{self, failed_attribution};
#[cfg(test)]
use batcher_cut::judge::{blocks_attribution, named_suites};
use batcher_cut::opts::{parse, Opts};
use batcher_cut::{endpass, io, order, pace, redtip, screen, vm};
mod telemetry;

use spira_otel::exit::ExitCode;
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use batcher::attrib::JobResult;
use batcher::core::{
    combine, cut_event, ejected_event, should_cut, skipped_event, stack_sequencing, stacked_into,
    stale_retry_due, topo_order, CombineInput, Ejection, Member, MergeResult, TriggerInputs,
    TriggerReason,
};
use io::{Env, Land, Repo};
use spira_config::process::{cfg, cfg_parse};

fn usage() -> ExitCode {
    eprintln!("usage: batcher cut <repo> [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher rounds|rounds-local|rounds-forge [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher sift <repo> [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher sifts [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher path-attribute <repo> --members id:tip,... --reds FILE [--record FILE] [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher attribute <repo> --members id:tip,... --wt DIR --base SHA [--batch ID] [--after-pass RESULTS] [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher pause-set --repo R --by concierge|operator --reason TEXT [--run DIR]");
    eprintln!("       batcher pause-show (--repo R [--held-since EPOCH] | --all) [--run DIR]");
    eprintln!("       batcher pause-lift --repo R --by concierge|operator --reason TEXT [--run DIR]");
    eprintln!("       batcher save-round --repo R --batch ID --out DIR [--run DIR] [--db DIR] [--home DIR]   (see: batcher save-round -h)");
    eprintln!("exit codes: 0 done (a lift with no pause says so and exits 0); 1 refused or failed; 2 usage");
    ExitCode::from(2)
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
        .map(|s| format!("batcher {repo}: rounds lane stopped on purpose by {} ({}) — no cut, and a landing does not lift it; lift with: queue round start {repo} --by <owner> --reason <why>", s.owner, s.reason))
}

fn cut_hold_note(queue_dir: &Path, repo: &str) -> Option<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    spira_config::cut_hold::held(queue_dir, repo, now)
        .map(|h| format!("batcher {repo}: cut held by {} ({}) — no cut until it lifts or its holder ends", h.owner, h.reason))
}

fn cut(o: &Opts) -> Result<(), pace::CutError> {
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
    if let Some(note) = cut_hold_note(&env_.queue_dir, &repo.name) {
        println!("{note}");
        return Ok(());
    }

    // The machine must answer before anything changes.
    if let Err(e) = io::lc_probe(&env_) {
        return Err(format!(
            "batcher cut {}: spira-lc is unreachable ({e}) — refused, nothing changed; fix the lifecycle machine",
            repo.name
        )
        .into());
    }

    // A forge batch the queue cannot read may still be open: the verdict settles it or says why.
    let forge_left = || io::read_open_batch(&env_, &repo.name).map_or(true, |o| o.is_some());
    if repo.land == Land::Local && forge_left() {
        if let Err(e) = settle_batch(&env_, &repo.name) {
            eprintln!(
                "batcher {}: forge batch left by a mode switch is unsettled: {e}",
                repo.name
            );
        }
    }

    let wait_secs = cfg_parse::<u64>("SPIRA_QUEUE_LOCK_WAIT")?;
    let Some(lock) = io::wait_lock(&env_, &repo.name, wait_secs)? else {
        return Err(format!(
            "{}: another operation holds the lock (waited {wait_secs}s)",
            repo.name
        )
        .into());
    };
    let _parked = io::park_queue_lock(lock);

    let (certified, pool_left) = io::certified_pool_left_out(&env_, &repo)?;
    let screened = screen::cut_pool(certified, &io::sifted_tips(&env_)?);
    let tips: std::collections::BTreeMap<String, String> =
        screened.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let (screened, withheld) = redtip::withhold(&env_.run, &repo.name, screened);
    for (id, tip, first) in &withheld {
        println!(
            "batcher {}: {id} left out: its tip {tip} is unchanged since a red attributed to it",
            repo.name
        );
        if *first {
            let note = format!("Left out of every round until its branch has a new commit: a red was attributed to spira/{id} at {tip} and the tip has not changed since.");
            match io::bead_note(&env_, id, &note) {
                Ok(()) => redtip::mark_filed(&env_.run, &repo.name, id, tip),
                Err(e) => eprintln!("batcher {}: {id} not noted: {e}", repo.name),
            }
        }
    }
    let (pool, kind) = batcher::core::select_round(batcher::core::base_fix_lane(screened));
    let (pool, first_blocked) = batcher::core::refuse_blocked(pool);
    let mut refused: Vec<String> = first_blocked.iter().map(|r| r.to_string()).collect();
    let (pool, full) = batcher::core::cap_round(batcher::core::topo_order(&pool), state_store::limits::MAX_CUT_MEMBERS);
    refused.extend(full);
    let (pool, blocked) = batcher::core::refuse_blocked(pool);
    let blocked_all: Vec<_> = first_blocked.into_iter().chain(blocked.iter().cloned()).collect();
    refused.extend(blocked.iter().map(|r| r.to_string()));
    for r in &refused {
        println!("batcher {}: {r}", repo.name);
    }
    io::announce_left_out(&env_, &repo, &io::cut_left_out(pool_left, &tips, &blocked_all));
    if let batcher::core::RoundKind::Feature(root) = &kind {
        println!(
            "batcher {}: feature round {root} ({} members)",
            repo.name,
            pool.len()
        );
    }
    let open = if repo.land == Land::Local {
        None
    } else {
        io::read_open_batch(&env_, &repo.name)?
    };

    if let Some(batch) = io::abandon_hold(&env_, &repo) {
        println!("batcher {}: round {batch} was not abandoned — no cut stacks on it; the abandon is retried each pass", repo.name);
        return Ok(());
    }

    let inputs = TriggerInputs {
        pool: &pool,
        main_red: false,
        batch_open: open.is_some(),
    };
    let Some(reason) = should_cut(&inputs) else {
        println!("{}", skipped_event("no trigger").text);
        return Ok(());
    };

    match open {
        Some(ob) => {
            println!(
                "batcher {}: batch PR {} is open — the next round is cut once it lands",
                repo.name, ob.pr
            );
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
        let base_files = if merges.get(&m.id) == Some(&MergeResult::Conflict) {
            io::base_conflict(repo, base_sha, &m.tip)
        } else {
            None
        };
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
    /// The deny list the round ran with; None for an attested round, whose pass ran elsewhere.
    deny: Option<vm::DenySnapshot>,
    /// (test, member) of each claimed row the round left standing.
    claimed: Vec<(String, String)>,
}

/// The effects of a round besides running suites (drive::RoundOps), on this box.
struct LiveOps<'a> {
    env: &'a Env,
    repo: &'a Repo,
    wt: &'a Path,
    start_sha: String,
    round_branch: String,
    evidence: String,
    pass: Option<PathBuf>,
    round: String,
    changed: BTreeMap<String, Vec<String>>,
    own_commits: vm::RoundCommits,
    tips: BTreeMap<String, String>,
    first_red: Option<u64>,
    key: String,
    batch: Option<&'a str>,
    prior: batcher::mender::Prior,
    send_back: Option<String>,
    mend_pass: std::cell::Cell<u32>,
    attributed_pass: Option<u32>,
    lifted: io::LiftedRows,
    returns_read: std::cell::RefCell<Option<(std::time::Instant, Vec<batcher::mender::Failure>)>>,
}

const MEND_DEADLINE_SECS: i64 = 900;

fn batch_pass(env: &io::Env, batch: &str) -> Result<u32, String> {
    let shown = io::lcq_held(env, &["show-batch", batch])?;
    serde_json::from_str::<serde_json::Value>(&shown)
        .ok()
        .and_then(|v| v.get("pass").and_then(serde_json::Value::as_u64))
        .map(|p| p as u32)
        .ok_or_else(|| format!("spira-lc show-batch {batch}: no pass in the reply"))
}

fn returns_of(
    pass: u32,
    rows: impl FnOnce(u32) -> Result<Vec<endpass::Row>, String>,
) -> Result<Vec<batcher::mender::Failure>, String> {
    Ok(endpass::returns(rows(pass)?))
}

impl LiveOps<'_> {
    fn keep_own_commits(&self) {
        if let Err(e) = vm::keep_round_commits(Path::new(&self.evidence), &self.own_commits) {
            eprintln!("batcher {}: could not keep the round's own commits for attribution: {e}", self.repo.name);
        }
    }

    fn mending(
        &mut self,
        batch: &str,
        d: &batcher::attrib::Decision,
        members: &[Member],
    ) -> Result<Option<drive::Mended>, String> {
        use batcher::mender::{self, Outcome};
        let shown = io::lcq_held(self.env, &["show-batch", batch])?;
        let pass = serde_json::from_str::<serde_json::Value>(&shown)
            .ok()
            .and_then(|v| v.get("pass").and_then(serde_json::Value::as_u64))
            .ok_or_else(|| format!("spira-lc show-batch {batch}: no pass in the reply"))?
            as u32;
        self.mend_pass.set(pass);
        let deadline = std::env::var("SPIRA_BATCHER_MEND_DEADLINE_SECS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(MEND_DEADLINE_SECS);
        let mut lc = endpass::LcMenders {
            env: self.env,
            batch,
            pass,
        };
        if endpass::Menders::rows(&mut lc)?.is_empty() {
            return Ok(None);
        }
        let failures = endpass::with_owners(endpass::wait(&mut lc, deadline)?, &d.owners);
        let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
        let mut mended = BTreeMap::new();
        let owners: Vec<String> = failures
            .iter()
            .filter(|f| f.outcome == Outcome::Mended)
            .flat_map(|f| f.members.clone())
            .collect();
        if !owners.is_empty() {
            let tips = io::lcq(self.env, &["mending", "tips", "--beads", &owners.join(",")])?;
            let tips: BTreeMap<String, Option<String>> = serde_json::from_str(&tips)
                .map_err(|e| format!("mending tips: unparsed reply: {e}"))?;
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
        Ok(Some(drive::Mended {
            next: decision.next,
            unattributed: decision.unattributed,
            suites: failures.into_iter().map(|f| f.suite).collect(),
        }))
    }
}

impl LiveOps<'_> {
    fn read_returns(&self, batch: &str) -> Result<Vec<batcher::mender::Failure>, String> {
        let pass = batch_pass(self.env, batch)?;
        self.mend_pass.set(pass);
        returns_of(pass, |pass| endpass::Menders::rows(&mut endpass::LcMenders { env: self.env, batch, pass }))
    }

    fn eject_with(
        &mut self,
        member: &Member,
        suites: &[String],
        fails: &[(String, String)],
        owner: bool,
        returned: Option<&str>,
    ) -> Result<(), String> {
        if owner {
            redtip::record(&self.env.run, &self.repo.name, &member.id, &member.tip);
            for suite in suites {
                let _ = spira_config::proof::record_failing_tests(&self.env.run, &member.id, suite, &[]);
            }
            let files = self.changed.get(&member.id).cloned().unwrap_or_default();
            io::record_ejection(self.env, &member.id, &member.tip, &files, (Path::new(&self.evidence), self.pass.as_deref()), suites, fails);
            io::reference_round_logs(self.env, &mut self.send_back, Path::new(&self.evidence), self.pass.as_deref(), &member.id);
        }
        match self.batch {
            Some(batch) => {
                let ejected = match returned {
                    Some(lead) => io::round_eject_returned(self.env, self.repo, batch, &member.id, suites, fails, lead),
                    None => io::round_eject(self.env, self.repo, batch, &member.id, suites, fails, owner),
                };
                if let Err(e) = ejected {
                    if io::HeldEject::parse(&e).is_some() {
                        println!("batcher {}: eject of {} from {batch} held: a pass of the round is running", self.repo.name, member.id);
                        return Err(e);
                    }
                    let refusal = format!("eject of {} from {batch} refused: {e}", member.id);
                    println!("batcher {}: {refusal}", self.repo.name);
                    return Err(refusal);
                }
            }
            None => io::eject_member(self.env, &self.repo.name, &member.id, suites, fails, owner),
        }
        println!(
            "{}",
            ejected_event(&Ejection {
                id: member.id.clone(),
                suites: suites.to_vec()
            })
            .text
        );
        self.lifted.restore_rows_of(&member.id, Path::new(&self.evidence))
    }
}

impl drive::RoundOps for LiveOps<'_> {
    fn suspects(&self, suite: &str, members: &[String]) -> Vec<String> {
        suspects_in(self.wt, &self.changed, suite, members)
    }

    fn eject(&mut self, member: &Member, suites: &[String], owner: bool) -> Result<(), String> {
        let fails: Vec<(String, String)> = suites
            .iter()
            .filter_map(|s| {
                io::suite_first_fail(Path::new(&self.evidence), self.pass.as_deref(), s).map(|l| (s.clone(), l))
            })
            .collect();
        self.eject_with(member, suites, &fails, owner, None)
    }

    fn eject_on(&mut self, member: &Member, filter: &str, line: &str, failed: &[String], owner: bool) -> Result<(), String> {
        self.eject_with(
            member,
            &[filter.to_string()],
            &[(filter.to_string(), line.to_string())],
            owner,
            None,
        )?;
        if owner && filter == "unit-tests" {
            if let Some(ids) = drive::flip_ids(failed) {
                if let Err(e) = spira_config::proof::record_failing_tests(&self.env.run, &member.id, filter, &ids) {
                    eprintln!("batcher: failing tests of {} not recorded: {e}", member.id);
                }
            }
        }
        Ok(())
    }

    fn eject_returned(&mut self, member: &Member, suites: &[String], why: &str) -> Result<(), String> {
        let fails: Vec<(String, String)> = suites
            .iter()
            .filter_map(|s| {
                io::suite_first_fail(Path::new(&self.evidence), self.pass.as_deref(), s).map(|l| (s.clone(), l))
            })
            .collect();
        let lead = io::returned_lead(suites, self.mend_pass.get(), self.batch.unwrap_or_default(), why);
        self.eject_with(member, suites, &fails, true, Some(&lead))
    }

    fn package_graph(&self) -> Option<batcher::unitattr::PackageGraph> {
        // EXTERNAL: cargo — the workspace's package graph
        let out = spira_config::bounded::bounded("cargo")
            .current_dir(self.wt)
            .args(["metadata", "--format-version", "1", "--no-deps", "--offline"])
            .output()
            .ok()?;
        out.status.success().then(|| io::package_graph(&String::from_utf8_lossy(&out.stdout))).flatten()
    }

    fn diffs(&self, members: &[String]) -> Vec<batcher::buildattr::MemberDiff> {
        let tips: BTreeMap<&str, &str> = self
            .tips
            .iter()
            .map(|(i, t)| (i.as_str(), t.as_str()))
            .collect();
        members
            .iter()
            .map(|id| batcher::buildattr::MemberDiff {
                id: id.clone(),
                paths: self.changed.get(id).cloned().unwrap_or_default(),
                added: tips
                    .get(id.as_str())
                    .map(|t| io::added_lines(self.repo, &self.start_sha, t))
                    .unwrap_or_default(),
            })
            .collect()
    }

    fn config_key_changes(&self, members: &[String]) -> Vec<(String, String)> {
        members
            .iter()
            .filter_map(|id| {
                let (added, retired) = io::config_key_changes(self.repo, &self.start_sha, self.tips.get(id)?);
                if added.is_empty() && retired.is_empty() {
                    return None;
                }
                Some((id.clone(), format!("adds: {}; retires: {}", added.join(","), retired.join(","))))
            })
            .collect()
    }

    fn sequence(&mut self, member: &Member, behind: &[String], line: &str) -> Result<(), String> {
        let outcome = match self.batch {
            Some(batch) => io::round_sequence(self.env, self.repo, batch, &member.id, behind, line),
            None => {
                self.eject_with(
                    member,
                    &[drive::WORKSPACE_BUILD.to_string()],
                    &[(drive::WORKSPACE_BUILD.to_string(), line.to_string())],
                    false,
                    None,
                )?;
                io::sequence_edges(self.env, &member.id, behind)
            }
        };
        match outcome {
            Ok(()) => println!(
                "batcher {}: {} held behind {} — {line}",
                self.repo.name,
                member.id,
                behind.join(",")
            ),
            Err(e) => {
                println!(
                    "batcher {}: could not hold {} behind {}: {e}",
                    self.repo.name,
                    member.id,
                    behind.join(",")
                );
                drive::RoundOps::incident(
                    self,
                    "workspace-build",
                    &[drive::WORKSPACE_BUILD.to_string()],
                );
            }
        }
        Ok(())
    }

    fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
        let rebuilt = merge_round(
            self.env,
            self.repo,
            self.wt,
            &self.start_sha,
            survivors.to_vec(),
            &self.own_commits,
        );
        self.keep_own_commits();
        rebuilt
    }

    fn fix_integration(&mut self, members: &[Member], suites: &[String]) -> bool {
        match io::integration_fix(self.env, self.wt, &self.start_sha, members) {
            Ok((fixes, sha)) if !fixes.is_empty() => {
                self.own_commits.borrow_mut().extend(sha);
                self.keep_own_commits();
                println!(
                    "batcher {}: integration red ({}) — round commit: {}",
                    self.repo.name,
                    suites.join(","),
                    fixes.join(", ")
                );
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
        self.changed = members
            .iter()
            .map(|m| (m.id.clone(), io::changed_paths(self.repo, &sha, &m.tip)))
            .collect();
        let rebuilt = merge_round(self.env, self.repo, self.wt, &sha, members.to_vec(), &self.own_commits)?;
        Ok(Some((sha, rebuilt)))
    }

    fn base_sha(&self) -> String {
        self.start_sha.clone()
    }

    fn repo_name(&self) -> String {
        self.repo.name.clone()
    }

    fn round_name(&self) -> String {
        self.batch
            .map_or_else(|| self.round.clone(), str::to_string)
    }

    fn record_unnamed_red(&mut self, name: &str, failed: &[String], members: &[Member]) {
        let red = drive::UnnamedRed {
            filter: name.to_string(),
            failed: failed.to_vec(),
            batch: self.round_name(),
            members: members
                .iter()
                .map(|m| (m.id.clone(), m.tip.clone()))
                .collect(),
        };
        redtip::keep_unnamed(&self.env.run, &self.repo.name, &red);
    }

    fn prior_unnamed_red(&self) -> Option<drive::UnnamedRed> {
        redtip::kept_unnamed(&self.env.run, &self.repo.name)
    }

    fn clear_unnamed_red(&mut self) {
        redtip::clear_unnamed(&self.env.run, &self.repo.name);
    }

    fn last_touching(&self, paths: &[String]) -> Option<String> {
        io::last_touching(self.repo, &self.start_sha, paths)
    }

    fn judge(&mut self, suites: &[String], members: &[String]) {
        let j = batcher::core::Judgement {
            source: batcher::core::RedSource::Local,
            suites: suites.to_vec(),
        };
        match io::file_judgement(self.env, self.repo, &j, members, &self.evidence) {
            Ok(id) => println!("{} — filed {id}", batcher::core::judgement_event(&j, &self.repo.name).text),
            Err(e) => println!("batcher {}: red {} not attributable to any member — could not file the judgement: {e}", self.repo.name, suites.join(",")),
        }
    }

    fn incident(&mut self, kind: &str, suites: &[String]) {
        let what = match kind {
            "base" => "base itself red",
            "unattributed" => "local red could not be attributed to anyone",
            spira_config::host_disk::CAUSE => "the host filesystem is full; no member is implicated",
            "integration" => "red only in the merged tree, not fixed in the round",
            "mender-page" => "the menders' passes ended in a box fault the loop cannot fix",
            drive::CONFIG_DELTA => "the merged config delta does not apply",
            drive::ATTRIBUTION_STALLED => "the attribution leg made no progress past its bound",
            drive::RUNNER_DIFFERENCE => "a unit tier red on one runner is green on the host it last ran green on; no member is implicated",
            _ => "workspace failed to build",
        };
        match io::file_local_red_incident(
            self.env,
            self.repo,
            suites,
            &self.round_branch,
            &self.evidence,
            kind,
        ) {
            Ok(id) => {
                println!(
                    "batcher {}: {what} ({}) — filed {id} for Ops",
                    self.repo.name,
                    suites.join(",")
                );
                if kind == "unattributed" || kind == "integration" {
                    if let Err(e) = io::write_hold(self.env, &self.repo.name, &self.key, &id) {
                        println!(
                            "batcher {}: could not record the hold on {id}: {e}",
                            self.repo.name
                        );
                    }
                }
            }
            Err(e) => println!(
                "batcher {}: {what} ({}) — could not file for Ops: {e}",
                self.repo.name,
                suites.join(",")
            ),
        }
    }

    fn adding(&self, suite: &str, members: &[batcher::core::Id]) -> Vec<batcher::core::Id> {
        let path = format!("spira/{suite}");
        members
            .iter()
            .filter(|m| {
                self.tips
                    .get(*m)
                    .is_some_and(|t| io::adds_path(&self.repo.path, &self.start_sha, t, &path))
            })
            .cloned()
            .collect()
    }

    fn touching(&self, suite: &str, members: &[batcher::core::Id]) -> Vec<batcher::core::Id> {
        let text = std::fs::read_to_string(self.wt.join("spira").join(suite)).unwrap_or_default();
        let covers = suite_select::header::covers_of(&text);
        members
            .iter()
            .filter(|m| {
                batcher::attrib::touches(
                    suite,
                    covers.as_deref(),
                    self.changed.get(*m).map_or(&[][..], |v| v),
                )
            })
            .cloned()
            .collect()
    }

    fn disable_flips(&mut self, tests: &[String]) -> Result<(), String> {
        let registry = spira_config::release_env::tool("test-registry")?;
        let (env, repo, branch) = (self.env, self.repo, self.round_branch.clone());
        let rows = io::disable_flips_with(
            &mut |id| io::file_flip_bead(env, repo, id, &branch),
            &mut io::through_registry(&registry, env),
            Path::new(&self.evidence),
            &branch,
            tests,
        )?;
        for r in rows {
            println!(
                "batcher {}: {} flipped — disabled until {}, follow-up {}",
                repo.name,
                r.test,
                io::date_of(r.until),
                r.bead
            );
        }
        Ok(())
    }

    fn lifted(&self) -> Vec<(String, batcher::core::Id)> {
        self.lifted.standing()
    }

    fn keep_lifted(&mut self, test: &str, owner: &batcher::core::Id, merged_lines: &[String], base_lines: &[String]) -> Result<String, String> {
        let (env, repo, wt, results) = (self.env, self.repo, self.wt, PathBuf::from(&self.evidence));
        let (round_branch, base, pass) = (self.round_branch.clone(), self.start_sha.clone(), self.pass.clone());
        let bead = self.lifted.keep(test, |old_bead| {
            let merged = match (merged_lines, test.strip_prefix("suite:")) {
                ([], Some(suite)) => io::suite_first_fail(&results, pass.as_deref(), suite).into_iter().collect(),
                _ => merged_lines.to_vec(),
            };
            let kept = io::KeptRow {
                test: test.to_string(),
                owner: owner.clone(),
                old_bead: old_bead.to_string(),
                round_branch,
                head: io::head_of(wt)?,
                base,
                merged_lines: merged,
                base_lines: base_lines.to_vec(),
            };
            let registry = spira_config::release_env::tool("test-registry")?;
            let row = io::keep_row_with(&mut |k| io::file_kept_row_bead(env, repo, k), &mut io::through_registry(&registry, env), &results, &kept)?;
            println!("batcher {}: {test} red on the merged tree and the base — row kept until {}, fix {}", repo.name, io::date_of(row.until), row.bead);
            Ok(row.bead)
        })?;
        Ok(bead)
    }

    fn record(&mut self, iteration: u32, d: &batcher::attrib::Decision) {
        for r in &d.records {
            self.first_red = Some(self.first_red.map_or(r.red_at, |f| f.min(r.red_at)));
            let fields = r.tsd_fields(&self.repo.name, &self.round, iteration);
            io::tsd_append(self.env, "round-attribution", &fields);
        }
    }

    fn settle_mending(
        &mut self,
        d: &batcher::attrib::Decision,
        members: &[Member],
    ) -> Option<Result<drive::Mended, String>> {
        let batch = self.batch?;
        self.mending(batch, d, members).transpose()
    }

    fn returned_for_attribution(&self) -> Vec<batcher::mender::Failure> {
        let (Some(batch), Some(pass)) = (self.batch, self.attributed_pass) else { return vec![] };
        self.mend_pass.set(pass);
        returns_of(pass, |pass| endpass::Menders::rows(&mut endpass::LcMenders { env: self.env, batch, pass })).unwrap_or_else(|e| {
            eprintln!("batcher {}: reading the mending rows of pass {pass}: {e}", self.repo.name);
            vec![]
        })
    }

    fn mender_returns(&self) -> Vec<batcher::mender::Failure> {
        let Some(batch) = self.batch else { return vec![] };
        let every = std::time::Duration::from_secs(endpass::POLL_SECS as u64);
        if let Some((at, read)) = &*self.returns_read.borrow() {
            if at.elapsed() < every {
                return read.clone();
            }
        }
        let read = self.read_returns(batch).unwrap_or_else(|e| {
            eprintln!("batcher {}: reading the mending rows mid-pass: {e}", self.repo.name);
            vec![]
        });
        *self.returns_read.borrow_mut() = Some((std::time::Instant::now(), read.clone()));
        read
    }

    fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>) {
        let paths = self
            .changed
            .get(&member.id)
            .cloned()
            .unwrap_or_default()
            .join(",");
        let evidence = format!("round {} evidence: {}", self.round, self.evidence);
        let rerun_rc = rerun.map(|r| if r == JobResult::Green { 0 } else { 1 });
        let report = io::record_escape(
            self.env,
            self.repo,
            self.wt,
            &self.start_sha,
            member,
            suite,
            &self.round,
            &paths,
            &evidence,
            rerun_rc,
        );
        println!("batcher {}: {report}", self.repo.name);
    }
}

/// The commit `rev` names in `repo`, if any.
fn git_rev(repo: &Repo, rev: &str) -> Option<String> {
    let o = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["rev-parse", "--verify", "-q", rev])
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// S's suspects: `# covers:` read from the round tree's own copy of the suite (the selector
/// crate's parser, sp-wx2tw), members ordered by attrib::suspect_order.
fn suspects_in(
    wt: &Path,
    changed: &BTreeMap<String, Vec<String>>,
    suite: &str,
    members: &[String],
) -> Vec<String> {
    let text = std::fs::read_to_string(wt.join("spira").join(suite)).unwrap_or_default();
    let covers = suite_select::header::covers_of(&text);
    batcher::attrib::suspect_order(suite, covers.as_deref(), members, changed)
}

/// Resets the round worktree to `start_sha` and merges `members` in order. Same closure rule
/// as the initial cut (core::combine): a member whose tip merged Empty because a stacked
/// dependent already carried it is kept; one Empty for any other reason, or a fresh
/// conflict, drops out.
fn merge_round(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    mut members: Vec<Member>,
    own: &vm::RoundCommits,
) -> Result<Vec<Member>, String> {
    io::worktree_reset(repo, wt, start_sha)?;
    let merges: BTreeMap<String, MergeResult> = members
        .iter()
        .map(|m| (m.id.clone(), io::merge_member(env_, wt, &m.id, &m.tip)))
        .collect();
    let before = members.clone();
    members.retain(|m| match merges.get(&m.id) {
        Some(MergeResult::Ok) => true,
        Some(MergeResult::Empty) => stacked_into(&before, &m.id),
        _ => false,
    });
    match io::fold_config_delta(env_, wt, &members) {
        Ok(sha) => own.borrow_mut().extend(sha),
        Err(e) => eprintln!("batcher {}: config delta not folded: {e}", repo.name),
    }
    Ok(members)
}

fn staged_worktree(env_: &Env, repo: &Repo) -> PathBuf {
    env_.run
        .join("worktree")
        .join(format!(".batcher-{}-staged", repo.name))
}

/// What stabilizing a round came to. `Emptied` means every culprit was ejected, so the pool
/// holds only survivors and the next cut is owed at once; `Blocked` means something is still red.
enum Stabilized {
    Stable(StableRound),
    Blocked,
    BaseRed { base: String, why: String },
    Emptied,
    Abandoned,
}

/// Enforces law-a-round-takes-certified-tips (amended 2026-09-27): a round that is red on its
/// own corpus is never sent on. The corpus runs on the round VM with concurrent attribution
/// (DESIGN.md §4): each red is attributed while the corpus still runs; every owner is ejected
/// with its suites; only those suites re-run on the survivors' release build. Flaky and base
/// reds do not block (a base red is filed for Ops); an unattributed red or a workspace that
/// does not build does. Returns `Blocked` or `Emptied` for a round that cannot land — the caller opens
/// no PR and changes no open-batch record, as if the round had never been cut.
fn append_round(env_: &Env, deny: Option<&vm::DenySnapshot>, fields: &[(&str, String)]) {
    let mut all = fields.to_vec();
    all.extend(deny.into_iter().flat_map(|d| d.fields()));
    io::tsd_append_round(env_, &all);
}

fn clear_claimed_rows(env_: &Env, repo: &Repo, round: &str, claims: &[(String, String)]) {
    if claims.is_empty() {
        return;
    }
    let registry = match spira_config::release_env::tool("test-registry") {
        Ok(r) => r,
        Err(e) => return println!("batcher {}: landed but claimed registry rows were not cleared: {e}", repo.name),
    };
    let failed = io::activate_claimed_with(&mut |test, reason| io::registry_activate(&registry, env_, test, reason), round, claims);
    for (test, why) in failed {
        println!("batcher {}: landed but claimed row {test} was not cleared: {why}", repo.name);
    }
}

fn resolve_deny(env_: &Env, at: u64) -> Result<vm::DenySnapshot, pace::CutError> {
    snapshot_from(io::deny_list_snapshot(env_, at), at)
}

fn snapshot_from(resolved: Result<String, String>, at: u64) -> Result<vm::DenySnapshot, pace::CutError> {
    let text = resolved
        .map_err(|e| pace::CutError::new(pace::CutKind::Other, format!("no round cut — the deny list could not be resolved: {e}")))?;
    Ok(vm::DenySnapshot { text, at, lifted: vec![], claimed: vec![] })
}

#[allow(clippy::too_many_arguments)]
fn stabilize_unlocked(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    starting: Vec<Member>,
    batch: Option<&str>,
    stage: &[Member],
    deny: &vm::DenySnapshot,
) -> Result<Stabilized, pace::CutError> {
    let wait_secs = cfg_parse::<u64>("SPIRA_QUEUE_LOCK_WAIT")?;
    io::unlocked(env_, &repo.name, batch, wait_secs, || {
        stabilize_round(env_, repo, wt, start_sha, starting, batch, stage, deny)
    })?
}

#[allow(clippy::too_many_arguments)]
fn stabilize_round(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    starting: Vec<Member>,
    batch: Option<&str>,
    stage: &[Member],
    deny: &vm::DenySnapshot,
) -> Result<Stabilized, pace::CutError> {
    let own_commits = vm::RoundCommits::default();
    let mut members = merge_round(env_, repo, wt, start_sha, starting, &own_commits)?;
    if let Some(batch) = batch {
        let held: Vec<(String, String)> = members
            .iter()
            .map(|m| (m.id.clone(), m.tip.clone()))
            .collect();
        let marked = io::gate_red_marks(env_, &held);
        if !marked.is_empty() {
            for (id, mark) in &marked {
                match io::round_eject_gate_red(env_, repo, batch, id, mark) {
                    Ok(()) => println!("batcher {}: ejected {id} from round {batch} — its own gate was red at this tip ({mark})", repo.name),
                    Err(e) => println!("batcher {}: could not eject gate-red {id} from round {batch}: {e}", repo.name),
                }
            }
            members.retain(|m| !marked.iter().any(|(id, _)| *id == m.id));
            members = merge_round(env_, repo, wt, start_sha, members, &own_commits)?;
        }
    }
    if members.is_empty() {
        println!(
            "{}",
            skipped_event("round emptied rebuilding the tree").text
        );
        return Ok(Stabilized::Emptied);
    }
    let round = now().to_string();
    let round_branch = format!("spira/batcher-attr/{}-{round}", repo.name);
    io::set_branch(repo, &round_branch, &io::head_of(wt)?)?;

    let history =
        std::fs::read_to_string(tsd::family_path(&env_.run, "suite-timing")).unwrap_or_default();
    let ordered = order::longest_first(&io::all_suites(repo, &round_branch)?, &history);
    if !ordered.unmeasured.is_empty() {
        println!(
            "batcher {}: ALARM no recorded wall time for {} — scheduled first",
            repo.name,
            ordered.unmeasured.join(",")
        );
    }
    let suites = ordered.list;
    if suites.is_empty() && repo.land == Land::Forge {
        let head = io::head_of(wt)?;
        return Ok(Stabilized::Stable(StableRound {
            members,
            head,
            green_branch: round_branch,
            attribution_seconds: None,
            regreen_seconds: None,
            staged: None,
            deny: None,
            claimed: vec![],
        }));
    }
    if suites.is_empty() && repo.land == Land::Local && gate::own::declares_own_testenv(wt) {
        let head = io::head_of(wt)?;
        return Ok(Stabilized::Stable(StableRound {
            members,
            head,
            green_branch: round_branch,
            attribution_seconds: None,
            regreen_seconds: None,
            staged: None,
            deny: None,
            claimed: vec![],
        }));
    }
    if suites.is_empty() {
        println!("batcher {}: round blocked — the merged tree has no test-*.sh suites; an empty corpus certifies nothing", repo.name);
        return Ok(Stabilized::Blocked);
    }
    let changed: BTreeMap<String, Vec<String>> = members
        .iter()
        .map(|m| (m.id.clone(), io::changed_paths(repo, start_sha, &m.tip)))
        .collect();

    let member_ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let rows = deny.test_rows();
    let claims: Vec<(String, String)> = members
        .iter()
        .flat_map(|m| io::member_claims(repo, start_sha, &m.tip, &rows).into_iter().map(|t| (t, m.id.clone())))
        .collect();
    let (lifted_deny, lifted) = deny.lifting(&member_ids, &claims);
    let deny = &lifted_deny;
    let mut runner =
        vm::VmRunner::new(env_, repo, wt, start_sha, &round, changed.clone())?.with_round_commits(own_commits.clone()).in_batch(batch).with_deny_list(deny)?;
    if let Err(e) = vm::keep_round_commits(&runner.results, &own_commits) {
        eprintln!("batcher {}: could not keep the round's own commits for attribution: {e}", repo.name);
    }
    let stage_bg = match batch {
        Some(_) if !stage.is_empty() && !env_.hypervisor.fits_a_staged_round() => {
            println!("batcher {}: no round staged — the hypervisor cannot hold the staged round beside this one and a CI runner", repo.name);
            None
        }
        Some(_) if !stage.is_empty() => {
            match io::round_stage_spawn(env_, repo, &staged_worktree(env_, repo), stage) {
                Ok(bg) => Some(bg),
                Err(e) => {
                    println!("batcher {}: {e} — no round staged", repo.name);
                    None
                }
            }
        }
        _ => None,
    };
    let mut ops = LiveOps {
        env: env_,
        repo,
        wt,
        start_sha: start_sha.to_string(),
        round_branch: round_branch.clone(),
        evidence: runner.results.display().to_string(),
        pass: None,
        round,
        changed,
        own_commits,
        tips: members
            .iter()
            .map(|m| (m.id.clone(), m.tip.clone()))
            .collect(),
        first_red: None,
        key: batcher::core::round_key(
            &members
                .iter()
                .map(|m| (m.id.clone(), m.tip.clone()))
                .collect::<Vec<_>>(),
        ),
        batch,
        prior: batcher::mender::Prior::default(),
        send_back: None,
        mend_pass: Default::default(),
        attributed_pass: None,
        lifted: io::LiftedRows::new(lifted),
        returns_read: Default::default(),
    };
    let budget = batcher::attrib::Budget::with_default(env_.maxpar, env_.round_slots);
    let round_members = members.clone();
    let end = drive::attribute_round(&mut runner, &mut ops, &suites, members, budget);
    if end.is_err() && runner.install_failed() {
        let fault = attribute_install(&mut runner, &round_members, &suites);
        install_fault_outcome(repo, &mut ops, &round_members, &fault);
    }
    let harness_fault = runner.harness_fault();
    runner.close();
    if let (Some(b), false) = (batch, matches!(end, Ok(drive::RoundEnd::Land { .. }))) {
        io::keep_round_results(&env_.run, b, &runner.results, io::rounds_prune(env_, repo, b).as_ref());
    }
    let staged = stage_bg.and_then(|bg| io::round_stage_done(repo, bg));
    let end = end.map_err(|e| {
        pace::CutError::new(
            if harness_fault {
                pace::CutKind::Infra
            } else {
                pace::CutKind::Other
            },
            e,
        )
    });
    match end? {
        drive::RoundEnd::Land {
            members,
            attribution_secs,
        } => {
            let head = io::head_of(wt)?;
            let green_branch = if git_rev(repo, &round_branch).as_deref() == Some(head.as_str()) {
                round_branch
            } else {
                let b = format!("{round_branch}-survivors");
                io::set_branch(repo, &b, &head)?;
                b
            };
            Ok(Stabilized::Stable(StableRound {
                members,
                head,
                green_branch,
                attribution_seconds: attribution_secs,
                regreen_seconds: ops.first_red.map(|f| now().saturating_sub(f)),
                staged,
                claimed: ops.lifted.standing_claims(),
                deny: Some(vm::DenySnapshot { lifted: deny.lifted.clone(), claimed: deny.claimed.clone(), ..vm::DenySnapshot::from_pass(&runner.results).unwrap_or_else(|| deny.clone()) }),
            }))
        }
        drive::RoundEnd::Blocked(why) => {
            println!("batcher {}: round blocked — {why}", repo.name);
            Ok(Stabilized::Blocked)
        }
        drive::RoundEnd::BaseRed { base, why } => {
            println!("batcher {}: base red at {base} — {why}", repo.name);
            Ok(Stabilized::BaseRed { base, why })
        }
        drive::RoundEnd::Faulted(why) | drive::RoundEnd::Stalled(why) => {
            println!("batcher {}: {why}", repo.name);
            Ok(Stabilized::Blocked)
        }
        drive::RoundEnd::RunnerDifference { red_on, green_on, .. } => {
            println!("batcher {}: red on {red_on}, green on {green_on} — a runner difference; no member charged", repo.name);
            Ok(Stabilized::Blocked)
        }
        drive::RoundEnd::Emptied(why) => {
            println!("batcher {}: round emptied — {why}", repo.name);
            Ok(Stabilized::Emptied)
        }
        drive::RoundEnd::Unconverged { red, probes, suspects } => {
            println!("batcher {}: attribution of {red} did not converge — {probes} probes, suspects {}", repo.name, suspects.join(","));
            Ok(Stabilized::Blocked)
        }
        drive::RoundEnd::Abandoned => {
            println!("batcher {}: the round was abandoned while it ran — stopped, no verdict written", repo.name);
            Ok(Stabilized::Abandoned)
        }
    }
}

/// The round's corpus died in the container install, so no suite is red to follow: bisect
/// the members on whether the install succeeds.
fn attribute_install(
    runner: &mut vm::VmRunner,
    members: &[Member],
    suites: &[String],
) -> batcher::attrib::InstallFault {
    let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let prereqs: BTreeMap<String, Vec<String>> = members
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                m.stack
                    .keys()
                    .filter(|p| ids.contains(p))
                    .cloned()
                    .collect(),
            )
        })
        .collect();
    drive::RoundRunner::set_members(runner, members);
    let suite = suites.first().cloned().unwrap_or_default();
    batcher::attrib::attribute_install_fault(
        &batcher::attrib::Shape::new(&ids, &prereqs),
        |removal| runner.probe_install(removal, &suite),
    )
}

/// Ejects the member the install fault names; a base or unattributed fault is filed for Ops.
/// Either way the round is not judged this pass — the error from the corpus run stands.
fn install_fault_outcome(
    repo: &Repo,
    ops: &mut LiveOps,
    members: &[Member],
    fault: &batcher::attrib::InstallFault,
) {
    use batcher::attrib::InstallFault;
    use drive::RoundOps;
    let install = vec!["install".to_string()];
    match fault {
        InstallFault::Owner(id) => {
            println!("batcher {}: install fault → owner {id}", repo.name);
            if let Some(m) = members.iter().find(|m| &m.id == id) {
                let _printed_refusal = ops.eject(m, &install, true);
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
) -> Result<Option<io::Promotion>, pace::CutError> {
    let head = io::head_of(wt)?;
    let named = io::named_ids(repo, base_sha, &head, &stable.members);
    let bins_ok = io::bins_present(repo, wt, &head) || gate::own::declares_own_testenv(wt);

    let refuse = |msg: String| {
        println!("{msg}");
        io::round_abandon(env_, repo, batch, &msg, "refused");
        stop_on_repeat(env_, &repo.name, batch, &msg);
        io::write_local_verdict(env_, &repo.name, "red", &msg);
        append_round(
            env_,
            stable.deny.as_ref(),
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
        return refuse(format!(
            "batcher {}: refused to land locally at {head} — {refusal}",
            repo.name
        ));
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
        println!(
            "batcher {}: the staged pass certified the round's tree (attested at {})",
            repo.name, stable.head
        );
    } else {
        let attest = if gate::own::declares_own_testenv(wt) {
            None
        } else {
            Some((
                stable.head.as_str(),
                io::all_suites(repo, &stable.green_branch)?.len(),
            ))
        };
        match io::round_certify(env_, repo, batch, attest, Some(&head)) {
            io::Certified::Green => println!(
                "batcher {}: certified the round's tree (round GREEN at {})",
                repo.name, stable.head
            ),
            io::Certified::Red { output } => {
                let text = std::fs::read_to_string(&output).unwrap_or_default();
                let changed: Vec<(String, Vec<String>)> = stable
                    .members
                    .iter()
                    .map(|m| (m.id.clone(), io::changed_paths(repo, base_sha, &m.tip)))
                    .collect();
                let guilty = io::implicated(&text, &changed);
                if guilty.is_empty() {
                    return refuse(format!(
                        "batcher {}: refused to land locally at {head} — the repository's ci is red and its output names no member's file ({output})",
                        repo.name
                    ));
                }
                let ci = io::ci_red_suites(wt);
                let mut ejected = 0;
                for id in &guilty {
                    if let Some(m) = stable.members.iter().find(|m| m.id == *id) {
                        redtip::record(&env_.run, &repo.name, id, &m.tip);
                    }
                    let paths = changed
                        .iter()
                        .find(|(m, _)| m == id)
                        .map(|(_, p)| p.as_slice())
                        .unwrap_or_default();
                    let first = text
                        .lines()
                        .find(|l| paths.iter().any(|p| l.contains(p.as_str())))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let fails = if first.is_empty() {
                        Vec::new()
                    } else {
                        vec![(ci[0].clone(), first)]
                    };
                    match io::round_eject(env_, repo, batch, id, &ci, &fails, true) {
                        Ok(()) => {
                            ejected += 1;
                            println!("batcher {}: ejected {id} from round {batch} — the repository's ci is red on its files ({output})", repo.name)
                        }
                        Err(e) => println!("batcher {}: could not eject {id} from round {batch}: {e}", repo.name),
                    }
                }
                io::round_abandon_ejecting(
                    env_,
                    repo,
                    batch,
                    &format!(
                        "ci red on {}; the rest are re-cut without them",
                        guilty.join(", ")
                    ),
                    "ci-red",
                    Some(ejected),
                );
                append_round(
                    env_,
                    stable.deny.as_ref(),
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
                let refused = refuse(format!(
                    "batcher {}: refused to land locally at {head} — cannot certify the round: {e}",
                    repo.name
                ));
                return match e.kind {
                    pace::CutKind::Infra => Err(e),
                    pace::CutKind::StaleImage => {
                        refused.map_err(|r| pace::CutError::new(e.kind, r.msg))
                    }
                    _ => refused,
                };
            }
        }
    }
    let stage_test =
        stable
            .staged
            .as_ref()
            .and_then(|_| match io::round_stage_test_spawn(env_, repo) {
                Ok(bg) => Some(bg),
                Err(e) => {
                    println!("batcher {}: {e}", repo.name);
                    None
                }
            });

    let certified: Vec<(String, String)> = stable
        .members
        .iter()
        .map(|m| (m.id.clone(), m.tip.clone()))
        .collect();
    io::lc_certify_round(env_, &certified, batch);

    let run = io::round_land(env_, repo, batch);
    let run = match run {
        Ok(r) => r,
        Err(e) => {
            if let Some(bg) = stage_test {
                io::round_stage_test_done(repo, bg);
            }
            return Err(e.into());
        }
    };
    let mut landed = run.outcome;
    let alarm = match landed {
        io::LandOutcome::Refused => Some(format!("refused: {}", run.refusal)),
        _ if !io::head_on_base(repo, &head) => {
            landed = io::LandOutcome::Refused;
            Some(format!(
                "queue round land exited as landed but {head} is not an ancestor of {}",
                repo.base
            ))
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
        io::round_abandon(
            env_,
            repo,
            batch,
            "queue round land refused — the round did not land",
            "land-refused",
        );
        io::write_local_verdict(
            env_,
            &repo.name,
            "red",
            "queue round land refused — see its own stderr above",
        );
        append_round(
            env_,
            stable.deny.as_ref(),
            &[
                ("repo", repo.name.clone()),
                ("verdict", "refused".to_string()),
                ("duration_ms", ((now() - round_start) * 1000).to_string()),
                ("base", base_sha.to_string()),
            ],
        );
        return Ok(None);
    }

    clear_claimed_rows(env_, repo, batch, &stable.claimed);

    match io::unpause_after_land(env_) {
        Ok(io::Unpause::Cleared) => println!("batcher {}: landed; cut pause cleared", repo.name),
        Ok(io::Unpause::Held) => println!(
            "batcher {}: landed; cut pause stays (deliberate hold)",
            repo.name
        ),
        Ok(_) => {}
        Err(e) => println!(
            "batcher {}: landed but the cut pause was not cleared: {e}",
            repo.name
        ),
    }
    io::write_local_verdict(env_, &repo.name, "green", "");
    io::close_superseded_reds(repo, &head);
    pace::Files::new(&env_.run, &repo.name).clear_refusals();
    println!(
        "batcher {}: landed locally at {head} — {} member(s)",
        repo.name,
        stable.members.len()
    );

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
    append_round(env_, stable.deny.as_ref(), &fields);

    let Some(bg) = stage_test else {
        return Ok(None);
    };
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
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&inbox)
                    .and_then(|mut f| f.write_all(line.as_bytes()));
            }
        }
        Ok(None) => {}
        Err(e) => println!("batcher {repo}: could not record the refusal: {e}"),
    }
}

fn cut_new_round(
    env_: &Env,
    repo: &Repo,
    pool: &[Member],
    reason: &TriggerReason,
) -> Result<(), pace::CutError> {
    let end = cut_new_round_inner(env_, repo, pool, reason);
    io::reap_attribution_branches("batcher", repo);
    end
}

fn cut_new_round_inner(
    env_: &Env,
    repo: &Repo,
    pool: &[Member],
    reason: &TriggerReason,
) -> Result<(), pace::CutError> {
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

    let wt = env_
        .run
        .join("worktree")
        .join(format!(".batcher-{}", repo.name));
    io::worktree_reset(repo, &wt, &base_sha)?;

    let mut merges = BTreeMap::new();
    for m in &sorted {
        merges.insert(m.id.clone(), io::merge_member(env_, &wt, &m.id, &m.tip));
    }
    let deleted = handle_base_conflicts(env_, repo, &sorted, &merges, &base_sha, base_moved_at);

    let winners: Vec<(String, String)> = sorted
        .iter()
        .filter(|m| merges.get(&m.id) == Some(&MergeResult::Ok))
        .map(|m| (m.id.clone(), m.tip.clone()))
        .collect();
    let mut siblings = BTreeMap::new();
    for m in &sorted {
        if merges.get(&m.id) != Some(&MergeResult::Conflict) || deleted.contains_key(&m.id) {
            continue;
        }
        let found = io::conflict_files(&wt, "HEAD", &m.tip)
            .and_then(|f| io::sibling_conflict(repo, &base_sha, &f, &winners));
        if let Some(found) = found {
            siblings.insert(m.id.clone(), found);
        }
    }

    let combined = combine(&CombineInput {
        pool: &sorted,
        merges: &merges,
        deleted_suites: &deleted,
        sequenced: &sequenced,
        siblings: &siblings,
    });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
        if sa.reason == batcher::core::SetAsideReason::Empty {
            if let Some(m) = sorted.iter().find(|m| m.id == sa.id) {
                io::lc_land_on_base(env_, repo, &m.id, &m.tip, &base_sha);
            }
        }
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("no members merged cleanly").text);
        return Ok(());
    }
    let key = batcher::core::round_key(
        &combined
            .merged
            .iter()
            .map(|m| (m.id.clone(), m.tip.clone()))
            .collect::<Vec<_>>(),
    );
    if let Some(bead) = io::hold_blocking(env_, &repo.name, &key) {
        println!("batcher {}: round held — same members at the same tips as the integration red {bead} still open; not re-cut", repo.name);
        return Ok(());
    }
    let deny = resolve_deny(env_, round_start)?;
    println!("{}", cut_event(reason, &combined).text);

    let batch = if repo.land == Land::Local {
        Some(io::round_open(env_, repo, &wt, &combined.merged)?)
    } else {
        None
    };
    let abandon = |why: &str, cause: &str| {
        if let Some(b) = batch.as_deref() {
            io::round_abandon(env_, repo, b, why, cause);
        }
    };

    let merged_ids: Vec<&str> = combined.merged.iter().map(|m| m.id.as_str()).collect();
    let stage_pool: Vec<Member> = sorted
        .iter()
        .filter(|m| !merged_ids.contains(&m.id.as_str()))
        .cloned()
        .collect();
    let stable = match stabilize_unlocked(
        env_,
        repo,
        &wt,
        &base_sha,
        combined.merged.clone(),
        batch.as_deref(),
        &stage_pool,
        &deny,
    ) {
        Err(e) => {
            abandon(&e.msg, "error");
            return Err(e);
        }
        Ok(Stabilized::Stable(s)) => s,
        Ok(Stabilized::Abandoned) => return Ok(()),
        Ok(Stabilized::Emptied) => {
            abandon(
                "every member was ejected; the survivors are re-cut",
                "emptied",
            );
            io::write_local_verdict(
                env_,
                &repo.name,
                io::RECUT_VERDICT,
                "every member of the round was ejected",
            );
            append_round(
                env_,
                Some(&deny),
                &[
                    ("repo", repo.name.clone()),
                    ("verdict", "emptied".to_string()),
                    ("duration_ms", ((now() - round_start) * 1000).to_string()),
                    ("base", base_sha),
                ],
            );
            return Ok(());
        }
        Ok(Stabilized::BaseRed { base, why }) => {
            abandon("local corpus red — held before reaching CI: base red", "blocked");
            io::write_local_verdict(env_, &repo.name, &pace::base_red_verdict(&base, &why), &why);
            append_round(
                env_,
                Some(&deny),
                &[
                    ("repo", repo.name.clone()),
                    ("verdict", "blocked".to_string()),
                    ("duration_ms", ((now() - round_start) * 1000).to_string()),
                    ("base", base_sha),
                ],
            );
            return Ok(());
        }
        Ok(Stabilized::Blocked) => {
            abandon("the round was blocked before it could land", "blocked");
            io::write_local_verdict(
                env_,
                &repo.name,
                "red",
                "local corpus red — held before reaching CI",
            );
            append_round(
                env_,
                Some(&deny),
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
        append_round(
            env_,
            Some(&deny),
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
        let mut next =
            finish_local_round(env_, repo, &wt, &base_sha, round_start, &stable, b, false)?;
        while let Some(p) = next {
            next = follow_promoted(env_, repo, pool, p)?;
        }
        return Ok(());
    }
    let batch_head = io::head_of(&wt)?;
    Ok(open_round_pr(
        env_,
        repo,
        &stable.members,
        &batch_head,
        round_start,
        stable.attribution_seconds,
        stable.regreen_seconds,
        stable.deny.as_ref(),
    )?)
}

/// The staged round `promote` cut after the round before it landed. Attested, its tree is the
/// one the staged pass judged and it lands on that; otherwise it gets its own pass here, as any
/// opened round does. Either way it ends as the rounds before it did, and may promote nothing
/// further: one stage is staged per cut.
fn follow_promoted(
    env_: &Env,
    repo: &Repo,
    pool: &[Member],
    p: io::Promotion,
) -> Result<Option<io::Promotion>, pace::CutError> {
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
        StableRound {
            members,
            head: p.head.clone(),
            green_branch: String::new(),
            attribution_seconds: None,
            regreen_seconds: None,
            staged: None,
            deny: None,
            claimed: vec![],
        }
    } else {
        let deny = resolve_deny(env_, round_start)?;
        match stabilize_unlocked(env_, repo, &wt, &base_sha, members, Some(&p.batch), &[], &deny) {
            Err(e) => {
                abandon(&e.msg, "error");
                return Err(e);
            }
            Ok(Stabilized::Stable(s)) => s,
            Ok(Stabilized::Abandoned) => return Ok(None),
            Ok(Stabilized::Emptied) => {
                abandon(
                    "every member was ejected; the survivors are re-cut",
                    "emptied",
                );
                io::write_local_verdict(
                    env_,
                    &repo.name,
                    io::RECUT_VERDICT,
                    "every member of the round was ejected",
                );
                return Ok(None);
            }
            Ok(Stabilized::BaseRed { base, why }) => {
                abandon("local corpus red — held before reaching CI: base red", "blocked");
                io::write_local_verdict(env_, &repo.name, &pace::base_red_verdict(&base, &why), &why);
                return Ok(None);
            }
            Ok(Stabilized::Blocked) => {
                abandon("the round was blocked before it could land", "blocked");
                io::write_local_verdict(
                    env_,
                    &repo.name,
                    "red",
                    "local corpus red — held before reaching CI",
                );
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
    finish_local_round(
        env_,
        repo,
        &wt,
        &base_sha,
        round_start,
        &stable,
        &p.batch,
        p.attested,
    )
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
    deny: Option<&vm::DenySnapshot>,
) -> Result<(), String> {
    batcher_cut::open_round_pr(
        env_,
        repo,
        merged,
        batch_head,
        round_start,
        attribution_seconds,
        regreen_seconds,
        deny,
    )?
    .1
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

/// Every registered repo's pause that has outlived one round's wall-clock bound with work waiting
/// is paged to the Concierge, whatever the repo's land mode.
fn page_stale_pauses(env_: &Env, repos: &[(String, String)]) {
    let Ok(inbox) = cfg("SPIRA_CONCIERGE_INBOX") else {
        return;
    };
    let inbox = PathBuf::from(inbox);
    let stamp = utc_stamp;
    for (name, _) in repos {
        let sm = pace::Seams {
            repo: name,
            now: now(),
            inbox: &inbox,
            stamp: &stamp,
            redrive_after: env_.orphan_redrive_after,
            pause_lift_after: env_.pause_lift_after,
            landing_head: None,
        };
        let waiting = || match find_repo(env_, name).and_then(|r| io::certified_pool(env_, &r)) {
            Ok(pool) => !pool.is_empty(),
            Err(e) => {
                eprintln!("batcher rounds: {name}: pool unreadable, a stale pause is not paged this pass: {e}");
                false
            }
        };
        if let Err(e) = pace::page_stale_pause(
            &pace::Files::new(&env_.run, name),
            &sm,
            env_.wall_secs,
            waiting,
        ) {
            eprintln!("batcher rounds: {name}: could not page a stale pause: {e}");
        }
    }
}

/// A held forge repo with CERTIFIED work says so in the Concierge inbox, once per change of the
/// waiting set, so a hold nobody remembers cannot sit on landable work in silence.
fn page_held(env_: &Env, name: &str) {
    let Ok(repo) = find_repo(env_, name) else {
        return;
    };
    let Ok(pool) = io::certified_pool(env_, &repo) else {
        return;
    };
    let marker = env_.run.join("rounds-held").join(name);
    if pool.is_empty() {
        let _ = std::fs::remove_file(&marker);
        return;
    }
    let ids = pool
        .iter()
        .map(|m| m.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(ids.as_str()) {
        return;
    }
    let Ok(inbox) = cfg("SPIRA_CONCIERGE_INBOX") else {
        return;
    };
    let line = format!("{} [watch:rounds] BATCHES HELD {name}: SPIRA_BATCHER_FORGE_ENABLE=0 with CERTIFIED work waiting: {ids}\n", utc_stamp(now()));
    use std::io::Write;
    let wrote = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&inbox)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if wrote.is_ok() && std::fs::create_dir_all(marker.parent().unwrap()).is_ok() {
        let _ = std::fs::write(&marker, &ids);
    }
}

fn settle_batch(env_: &Env, name: &str) -> Result<(), String> {
    // batch-job: the verdict waits on CI and a red batch's replay; it ends when the batch settles
    // FORK-EDGE: machine leg queue verdict
    let st = std::process::Command::new(&env_.queue_bin)
        .args(["verdict", name])
        .status()
        .map_err(|e| format!("queue verdict: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!(
            "queue verdict exited {}",
            st.code().map_or("by signal".to_string(), |c| c.to_string())
        ))
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
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn paced_cut(env_: &Env, name: &str, one: &Opts) -> Result<(), pace::CutError> {
    let repo = find_repo(env_, name)?;
    let inbox = PathBuf::from(cfg("SPIRA_CONCIERGE_INBOX")?);
    let stamp = utc_stamp;
    let sm = pace::Seams {
        repo: name,
        now: now(),
        inbox: &inbox,
        stamp: &stamp,
        redrive_after: env_.orphan_redrive_after,
        pause_lift_after: env_.pause_lift_after,
        landing_head: io::resolve_base_sha(&repo).ok(),
    };
    let (fit, redrive, incident) = (
        |id: &str| io::round_certifiable(env_, &repo, id),
        |id: &str| io::redrive_round(&repo, id),
        |id: &str, why: &str| io::file_orphan_round_incident(env_, &repo, id, why),
    );
    let opened = |id: &str| io::round_is_opened(env_, &repo, id);
    let orphans = pace::Orphans {
        fit: &fit,
        redrive: &redrive,
        incident: &incident,
        opened: &opened,
    };
    pace::paced(
        &pace::Files::new(&env_.run, name),
        &sm,
        || io::open_round_id(env_, &repo),
        |id| io::certify_driven(env_, name, id),
        || io::read_local_verdict(env_, name),
        &orphans,
        || cut(one),
    )
}

/// One scheduler pass over the repo map. `Pass::Local` is the harness's VM rounds, run one repo
/// at a time; `Pass::Forge` settles and opens queue.forge batches. They run as separate units so
/// a long VM round never delays a forge batch. `SPIRA_BATCHER_ENABLE=0` holds only the home
/// repository's local rounds; `SPIRA_BATCHER_FORGE_ENABLE=0` holds only the forge batches.
fn rounds(o: &Opts, pass: Pass) -> Result<(), String> {
    let on = Switches {
        local: cfg("SPIRA_BATCHER_ENABLE")?.trim() != "0",
        forge: cfg("SPIRA_BATCHER_FORGE_ENABLE")?.trim() != "0",
    };
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
    let repos: Vec<(String, String)> = reg
        .all()
        .into_iter()
        .map(|n| {
            let l = reg.land(&n);
            (n, l)
        })
        .collect();
    if !on.forge && pass != Pass::Local {
        for (name, _) in repos.iter().filter(|(_, l)| l == "queue") {
            page_held(&env_, name);
        }
    }
    page_stale_pauses(&env_, &repos);
    let failed = drive_pass(&repos, reg.home_repo(), pass, on, |name, step| match step {
        Step::Verdict => {
            let armed = find_repo(&env_, name).and_then(|r| io::forge_arm_queue(&r));
            settle_batch(&env_, name).and(armed)
        }
        Step::Cut => {
            let one = Opts {
                cmd: "cut".into(),
                repo: name.to_string(),
                run: o.run.clone(),
                db: o.db.clone(),
                home: o.home.clone(),
                round_vm: o.round_vm.clone(),
                suites: None,
                members: None,
                evidence: None,
                reds: None,
        after_pass: None,
        confirmed: false,
                record: None,
                wt: None,
                base: None,
                batch: None,
                by: None,
                reason: None,
                all: false,
                held_since: None, out: None,
            };
            if paced && reg.land(name) == "queue.local" {
                paced_cut(&env_, name, &one).map_err(|e| e.msg)
            } else {
                cut(&one).map_err(|e| e.msg)
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
    println!(
        "batcher {}: sift: {} sifted, {} sent back, {} superseded, {} held",
        repo.name,
        out.sifted.len(),
        out.sent_back.len(),
        out.superseded.len(),
        out.capped.len() + out.held.len()
    );
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
        let one = Opts {
            cmd: "sift".into(),
            repo: name.clone(),
            run: o.run.clone(),
            db: o.db.clone(),
            home: o.home.clone(),
            round_vm: o.round_vm.clone(),
            suites: None,
            members: None,
            evidence: None,
            reds: None,
        after_pass: None,
        confirmed: false,
            record: None,
            wt: None,
            base: None,
            batch: None,
            by: None,
            reason: None,
            all: false,
            held_since: None, out: None,
        };
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
    let suites: Vec<String> = o
        .suites
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let members: Vec<String> = o
        .members
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
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
    let failed = attribute_run(o);
    if let Err(why) = &failed {
        let ended = match (o.batch.as_deref(), o.run.as_deref()) {
            (Some(batch), Some(run)) => spira_config::round_ended::is_ended(run, batch),
            _ => false,
        };
        let outcome = failed_attribution(ended, why.clone());
        println!("{}", outcome.line());
        if ended {
            return failed;
        }
        if let (Some(batch), Some(home), Some(run)) = (o.batch.as_deref(), o.home.clone(), o.run.clone()) {
            if let Ok(env_) = env_for(o, home, run) {
                if let Ok(repo) = find_repo(&env_, &o.repo) {
                    io::round_attribution_failed(&env_, &repo, batch, why);
                }
            }
        }
    }
    failed
}

fn attribute_run(o: &Opts) -> Result<(), String> {
    use batcher_cut::attribute::parse_members;
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
    println!("{}", attribute_in(o, env_, repo, wt, start_sha, given)?);
    Ok(())
}

fn attribute_in(
    o: &Opts,
    env_: Env,
    repo: Repo,
    wt: PathBuf,
    start_sha: String,
    given: Vec<(String, String)>,
) -> Result<String, String> {
    // The pool's own rows carry each member's stack and blockers (the attribution shape); a
    // member the pool no longer holds is attributed as an unstacked one at the given tip.
    let pool = io::certified_pool(&env_, &repo).unwrap_or_default();
    let members: Vec<Member> = given
        .iter()
        .map(|(id, tip)| {
            let known = pool.iter().find(|m| &m.id == id).cloned();
            Member {
                tip: tip.clone(),
                ..known.unwrap_or_else(|| judge::bare_member(id, tip))
            }
        })
        .collect();
    let round = now().to_string();
    let round_branch = format!("spira/batcher-attr/{}-{round}", repo.name);
    let history = std::fs::read_to_string(tsd::family_path(&env_.run, "suite-timing")).unwrap_or_default();
    let listed = io::set_branch(&repo, &round_branch, &io::head_of(&wt)?).and_then(|()| io::all_suites(&repo, &round_branch));
    let budget = batcher::attrib::Budget::with_default(env_.maxpar, env_.round_slots);
    let mut edge = LiveEdge { o, env_: &env_, repo: &repo, wt: &wt, start_sha, round_branch, round, given };
    judge::attribute(o, &mut edge, members, listed, &history, budget)
}

struct LiveEdge<'a> {
    o: &'a Opts,
    env_: &'a Env,
    repo: &'a Repo,
    wt: &'a Path,
    start_sha: String,
    round_branch: String,
    round: String,
    given: Vec<(String, String)>,
}

impl<'a> judge::Edge for LiveEdge<'a> {
    type Runner = vm::VmRunner<'a>;
    type Ops = LiveOps<'a>;

    fn changed(&self, members: &[Member]) -> BTreeMap<String, Vec<String>> {
        members.iter().map(|m| (m.id.clone(), io::changed_paths(self.repo, &self.start_sha, &m.tip))).collect()
    }

    fn current_deny(&self) -> Result<vm::DenySnapshot, String> {
        resolve_deny(self.env_, now()).map_err(|e| e.msg)
    }

    fn open(&mut self, _suites: &[String], changed: BTreeMap<String, Vec<String>>, deny: &vm::DenySnapshot, own_commits: vm::RoundCommits) -> Result<(Self::Runner, Self::Ops), String> {
        let (o, env_, repo) = (self.o, self.env_, self.repo);
        if let Some(pass) = o.after_pass.as_deref().filter(|p| !p.join(vm::ROUND_COMMITS_FILE).is_file()) {
            eprintln!("batcher {}: {} holds no {}; the round's own commits are unknown to this attribution", repo.name, pass.display(), vm::ROUND_COMMITS_FILE);
        }
        let runner = vm::VmRunner::new(env_, repo, self.wt, &self.start_sha, &self.round, changed.clone())?
            .with_round_commits(own_commits.clone())
            .in_batch(o.batch.as_deref())
            .attribution()
            .after_pass(o.after_pass.clone())
            .with_deny_list(deny)?
            .confirmed(o.confirmed);
        let attributed_pass = o.batch.as_deref().zip(o.after_pass.as_deref()).and_then(|(b, _)| {
            batch_pass(env_, b).map_err(|e| eprintln!("batcher {}: the attributed pass is unknown, so a mender's return goes untried: {e}", repo.name)).ok()
        });
        if let (Some(b), Some(pass)) = (o.batch.as_deref(), o.after_pass.as_deref()) {
            io::round_corpus_ended_red(env_, repo, b);
            if pass.is_dir() {
                io::keep_attributed_pass(&env_.run, b, pass, io::rounds_prune(env_, repo, b).as_ref());
            }
        }
        let ops = LiveOps {
            env: env_,
            repo,
            wt: self.wt,
            start_sha: self.start_sha.clone(),
            round_branch: self.round_branch.clone(),
            evidence: runner.results.display().to_string(),
            pass: o.after_pass.clone(),
            round: self.round.clone(),
            changed,
            own_commits,
            tips: self.given.iter().cloned().collect(),
            first_red: None,
            key: batcher::core::round_key(&self.given),
            batch: o.batch.as_deref(),
            prior: batcher::mender::Prior::default(),
            send_back: None,
            mend_pass: Default::default(),
            attributed_pass,
            lifted: io::LiftedRows::default(),
            returns_read: Default::default(),
        };
        Ok((runner, ops))
    }

    fn settle(&mut self, runner: &mut Self::Runner, ops: &mut Self::Ops, end: Result<drive::RoundEnd, String>, members: &[Member], suites: &[String]) -> Result<drive::RoundEnd, String> {
        let (o, env_, repo) = (self.o, self.env_, self.repo);
        if end.is_err() && runner.host_disk() {
            println!("batcher {}: install fault → {}", repo.name, spira_config::host_disk::CAUSE);
            drive::RoundOps::incident(ops, spira_config::host_disk::CAUSE, &["install".to_string()]);
        } else if end.is_err() && runner.install_failed() {
            let fault = attribute_install(runner, members, suites);
            install_fault_outcome(repo, ops, members, &fault);
        }
        runner.close();
        let end = match (&end, o.batch.as_deref()) {
            (Ok(drive::RoundEnd::Land { .. }), Some(b)) => match io::keep_attribution_deny_list(&env_.run, b, &runner.results) {
                Ok(()) => end,
                Err(e) => Ok(drive::RoundEnd::Blocked(format!("the attribution's deny list could not be kept for the round: {e}"))),
            },
            _ => end,
        };
        if let (Some(b), false) = (o.batch.as_deref(), matches!(end, Ok(drive::RoundEnd::Land { .. }))) {
            io::keep_attribution_results(&env_.run, b, &runner.results, o.after_pass.is_some(), io::rounds_prune(env_, repo, b).as_ref());
        }
        end
    }

    fn mended(&self, ops: &Self::Ops) -> bool {
        ops.prior != batcher::mender::Prior::default()
    }

    fn head(&self) -> String {
        io::head_of(self.wt).unwrap_or_default()
    }
}

fn changed_paths(repo: &Repo, tip: &str) -> Result<Vec<String>, String> {
    let git = |args: &[&str]| {
        let o = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&repo.path)
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&o.stderr).trim()
            ))
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
    let mut reds = parse_reds(
        &std::fs::read_to_string(reds_file).map_err(|e| format!("{}: {e}", reds_file.display()))?,
    )?;
    let env_ = env_for(o, home, run)?;
    let repo = find_repo(&env_, &o.repo)?;
    for r in &mut reds {
        if r.own.is_empty() {
            r.own = suite_own(&repo, &r.suite);
        }
    }
    let mut members = Vec::new();
    for spec in o
        .members
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (id, tip) = spec
            .split_once(':')
            .ok_or_else(|| format!("--members: {spec:?} is not id:tip"))?;
        members.push(MemberPaths {
            id: id.to_string(),
            paths: changed_paths(&repo, tip)?,
        });
    }
    let verdict = attribute(&reds, &members);
    let (code, line) = match &verdict {
        Verdict::Eject(owners) => {
            println!("verdict=auto-eject");
            for (id, why) in owners {
                println!("eject={id}\t{why}");
            }
            (
                0,
                format!(
                    "auto-eject {}",
                    owners
                        .iter()
                        .map(|(i, _)| i.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            )
        }
        Verdict::Ambiguous(rows) => {
            println!("verdict=ambiguous");
            for r in rows {
                let c: Vec<String> = r
                    .candidates
                    .iter()
                    .map(|(id, p)| format!("{id} [{}]", p.join(", ")))
                    .collect();
                println!(
                    "red={}\tcandidates={}",
                    r.suite,
                    if c.is_empty() {
                        "none".into()
                    } else {
                        c.join(" | ")
                    }
                );
            }
            (
                3,
                format!(
                    "ambiguous {}",
                    rows.iter()
                        .map(|r| r.suite.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            )
        }
    };
    if let Some(rec) = &o.record {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(rec)
            .map_err(|e| format!("{}: {e}", rec.display()))?;
        writeln!(f, "{}\t{}\t{line}", now(), repo.name)
            .map_err(|e| format!("{}: {e}", rec.display()))?;
    }
    Ok(ExitCode::from(code))
}

fn main() -> std::process::ExitCode {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("batcher-cut: {e}");
        std::process::exit(2);
    }
    let first = std::env::args().nth(1);
    spira_otel::exit::finish(telemetry::run(&telemetry::sink(), first.as_deref(), || {
        dispatch().code()
    }))
}

fn person_flags(o: &Opts) -> Result<(String, String, String), String> {
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("--repo is required".into());
    }
    let by = o.by.clone().ok_or("--by is required")?;
    if !spira_otel::rounds::ACTORS.contains(&by.as_str()) || !spira_otel::rounds::is_hand(&by) {
        return Err(format!("--by {by:?} must be concierge or operator"));
    }
    let reason = o.reason.clone().filter(|r| !r.trim().is_empty()).ok_or("--reason is required")?;
    Ok((run.to_string_lossy().into_owned(), by, reason))
}

fn pause_set(o: &Opts, out: &mut dyn std::io::Write) -> Result<(), String> {
    let (run, by, reason) = person_flags(o)?;
    let text = format!("hold:{}", pace::one_line(&reason));
    pace::Files::new(Path::new(&run), &o.repo).set_pause(&text, &by, &reason, now())?;
    writeln!(out, "batcher: {}: paused by {by} — {text}", o.repo).map_err(|e| e.to_string())
}

fn pause_lift(o: &Opts, out: &mut dyn std::io::Write) -> Result<(), String> {
    let (run, by, reason) = person_flags(o)?;
    let line = match pace::Files::new(Path::new(&run), &o.repo).lift_pause(&by, &reason, now())? {
        Some(held) => format!("batcher: {}: pause lifted by {by} after {held}s", o.repo),
        None => format!("batcher: {}: there was no pause", o.repo),
    };
    writeln!(out, "{line}").map_err(|e| e.to_string())
}

fn pause_show(o: &Opts, out: &mut dyn std::io::Write) -> Result<(), String> {
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let repos: Vec<String> = if o.all {
        let mut names: Vec<String> = std::fs::read_dir(run.join("rounds-pace"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    } else if o.repo.is_empty() {
        return Err("--repo or --all is required".into());
    } else {
        vec![o.repo.clone()]
    };
    let mut shown = false;
    for r in &repos {
        let files = pace::Files::new(&run, r);
        if let Some(text) = files.pause_text() {
            shown = true;
            if o.held_since.is_some() && files.pause_set_at() == o.held_since {
                return Err(format!("{r}: the pause set at {} still holds ({})", o.held_since.unwrap_or_default(), text.trim()));
            }
            let kind = if text.starts_with("hold:") { "hold" } else { "red" };
            let since = files.pause_set_at().map_or("unknown".to_string(), |t| t.to_string());
            writeln!(out, "{r}: paused kind={kind} since={since} text={}", text.trim()).map_err(|e| e.to_string())?;
        } else if !o.all {
            writeln!(out, "{r}: no pause").map_err(|e| e.to_string())?;
        }
    }
    if o.all && !shown {
        writeln!(out, "no repo is paused").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn save_round(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let batch = o.batch.as_deref().ok_or("--batch is required")?;
    let out = o.out.as_deref().ok_or("--out is required")?;
    if o.repo.is_empty() {
        return Err("--repo is required".into());
    }
    let env_ = env_for(o, home.clone(), run.clone())?;
    let repo = find_repo(&env_, &o.repo)?;
    let batches = io::lcq_held(&env_, &["list", "--batches", "--batch", batch])?;
    let results = io::round_results_dir(&run, batch);
    let deny = std::fs::read_to_string(home.join("spira/inventory-deny")).map(|t| spira_lint::rules::inventory::deny_fragments(&t)).unwrap_or_default();
    let built = batcher_cut::replay::build(&batches, batch, &results, &batcher_cut::replay::RepoGit(&repo), &batcher_cut::replay::names_here(&run), &deny).map_err(|e| batcher_cut::replay::refuse(out, e))?;
    batcher_cut::replay::write(&built, out)?;
    println!("batcher: round {batch} saved to {} ({} files)", out.display(), built.files.len());
    Ok(())
}

fn dispatch() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("save-round") && std::env::args().nth(2).is_some_and(|a| a == "-h" || a == "--help") {
        println!("{}", batcher_cut::replay::USAGE);
        return ExitCode::SUCCESS;
    }
    let o = match parse(&env::args().collect::<Vec<_>>()).and_then(|o| o.with_ambient(env::var_os("SPIRA_HOME").map(PathBuf::from))) {
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
        "cut" => cut(&o).map_err(|e| e.msg),
        "rounds" => rounds(&o, Pass::All),
        "rounds-local" => rounds(&o, Pass::Local),
        "rounds-forge" => rounds(&o, Pass::Forge),
        "judgement-ci" => judgement_ci(&o),
        "attribute" => attribute(&o),
        "sift" => sift_one(&o),
        "sifts" => sifts(&o),
        "pause-set" => pause_set(&o, &mut std::io::stdout()),
        "pause-show" => pause_show(&o, &mut std::io::stdout()),
        "pause-lift" => pause_lift(&o, &mut std::io::stdout()),
        "save-round" => save_round(&o),
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

    #[test_id(ts0qpfv)]
    #[test]
    fn a_failed_attribution_of_an_ended_round_is_abandoned_not_blocked() {
        use batcher_cut::attribute::Attribution;
        assert_eq!(failed_attribution(true, "x".into()), Attribution::Abandoned);
        assert_eq!(failed_attribution(false, "x".into()), Attribution::Blocked("x".into()));
    }

    #[test_id(t2jxdat)]
    #[test]
    fn a_returned_row_of_the_attributed_pass_is_read_by_its_pass_number_not_the_current_one() {
        let row = |state: &str| endpass::Row {
            suite: "test-spike.sh".into(),
            state: state.into(),
            deadline: None,
            diagnosis: "loses the note".into(),
            returned: vec!["m3".into()],
            ask_open: false,
        };
        let by_pass = |pass: u32| Ok(if pass == 1 { vec![row("RETURNED")] } else { vec![] });
        let current = 2;
        assert!(returns_of(current, by_pass).unwrap().is_empty());
        let got = returns_of(1, by_pass).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].members, ["m3"]);
    }

    fn pause_opts(run: &Path, by: Option<&str>) -> Opts {
        Opts {
            cmd: String::new(),
            repo: "a".into(),
            run: Some(run.to_path_buf()),
            db: None,
            home: None,
            round_vm: None,
            suites: None,
            members: None,
            evidence: None,
            reds: None,
            after_pass: None,
        confirmed: false,
            record: None,
            wt: None,
            base: None,
            batch: None,
            by: by.map(String::from),
            reason: Some("eject done".into()),
            all: false,
            held_since: None, out: None,
        }
    }

    fn red_pause(run: &Path) -> pace::Files {
        let f = pace::Files::new(run, "a");
        std::fs::create_dir_all(f.pause_file().parent().unwrap()).unwrap();
        std::fs::write(f.pause_file(), "red:1\n").unwrap();
        f
    }

    #[test_id(tfkx6c6)]
    #[test]
    fn sp_plukb4_pause_show_held_since_fails_only_while_that_pause_holds() {
        let d = testkit::TempDir::new("batcher-pause-held-since");
        let f = red_pause(d.path());
        let since = |n| Opts { held_since: Some(n), ..pause_opts(d.path(), None) };
        assert!(pause_show(&since(1), &mut Vec::new()).is_err(), "the pause set at 1 holds");
        assert!(pause_show(&since(2), &mut Vec::new()).is_ok(), "another since is no longer this pause");
        std::fs::remove_file(f.pause_file()).unwrap();
        assert!(pause_show(&since(1), &mut Vec::new()).is_ok(), "no pause holds");
    }

    #[test_id(t1me1ff)]
    #[test]
    fn pause_lift_by_the_concierge_lifts_and_records_the_concierge() {
        let d = testkit::TempDir::new("batcher-pause-lift");
        let f = red_pause(d.path());
        let mut out = Vec::new();
        pause_lift(&pause_opts(d.path(), Some("concierge")), &mut out).unwrap();
        assert!(!f.pause_file().exists());
        let log = std::fs::read_to_string(d.path().join("landing.log")).unwrap();
        assert!(log.contains("QUEUE ROUND-PAUSE-LIFT") && log.contains("by=concierge"), "{log}");
    }

    #[test_id(tms6no3)]
    #[test]
    fn pause_lift_without_a_valid_actor_is_refused_naming_the_flag_and_the_pause_stands() {
        let d = testkit::TempDir::new("batcher-pause-refuse");
        let f = red_pause(d.path());
        for by in [None, Some("dwarf"), Some("batcher")] {
            let e = pause_lift(&pause_opts(d.path(), by), &mut Vec::new()).unwrap_err();
            assert!(e.contains("--by"), "{e}");
            assert!(f.pause_file().exists());
        }
        let mut o = pause_opts(d.path(), Some("operator"));
        o.reason = None;
        assert!(pause_lift(&o, &mut Vec::new()).unwrap_err().contains("--reason"));
        assert!(f.pause_file().exists());
    }

    #[test_id(txv0gyg)]
    #[test]
    fn pause_set_by_a_person_writes_a_hold_the_batcher_neither_pages_nor_lifts() {
        let d = testkit::TempDir::new("batcher-pause-set");
        pause_set(&pause_opts(d.path(), Some("operator")), &mut Vec::new()).unwrap();
        let f = pace::Files::new(d.path(), "a");
        assert_eq!(std::fs::read_to_string(f.pause_file()).unwrap(), "hold:eject done");
        assert_eq!(f.pause_since(), None);
        let log = std::fs::read_to_string(d.path().join("landing.log")).unwrap();
        assert!(log.contains("by=operator kind=hold"), "{log}");
        let recs = spira_otel::logs::read_all(&spira_otel::dir_of(d.path()), "batcher");
        let has = |k: &str, v: &str| recs.iter().any(|r| r.iter().any(|(n, x)| n == k && x["stringValue"] == v));
        assert!(has("kind", "pause") && has("by", "operator"));
    }

    #[test_id(tr5qonk)]
    #[test]
    fn a_person_lifts_their_own_hold_and_show_reads_it_first() {
        let d = testkit::TempDir::new("batcher-pause-hold-lift");
        let mut o = pause_opts(d.path(), Some("operator"));
        o.reason = Some("two\nlines".into());
        pause_set(&o, &mut Vec::new()).unwrap();
        let f = pace::Files::new(d.path(), "a");
        assert_eq!(std::fs::read_to_string(f.pause_file()).unwrap(), "hold:two lines");
        let mut out = Vec::new();
        pause_show(&o, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("kind=hold"));
        pause_lift(&o, &mut Vec::new()).unwrap();
        assert!(!f.pause_file().exists());
    }

    #[test_id(t950gna)]
    #[test]
    fn pause_lift_with_no_pause_says_so_and_succeeds_and_show_reads_the_pause() {
        let d = testkit::TempDir::new("batcher-pause-none");
        let mut out = Vec::new();
        pause_lift(&pause_opts(d.path(), Some("concierge")), &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("no pause"));
        let mut o = pause_opts(d.path(), None);
        let mut out = Vec::new();
        pause_show(&o, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("no pause"));
        red_pause(d.path());
        o.all = true;
        let mut out = Vec::new();
        pause_show(&o, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("a: paused kind=red since=1"));
    }

    #[test_id(ton3tex)]
    #[test]
    fn an_empty_suite_list_does_not_block_attribution_of_a_red_test_tier() {
        let dir = testkit::TempDir::new("batcher-cut-red-tier");
        std::fs::write(dir.path().join("integration.json"), r#"{"stage":"integration-tests","verdict":"red","failed":["x"]}"#).unwrap();
        let red = io::red_test_phases(dir.path());
        assert_eq!(red, ["integration-tests"]);
        assert_eq!(blocks_attribution(&[], &red, &[], Some(dir.path())), None);
        let green = testkit::TempDir::new("batcher-cut-green-tier");
        std::fs::write(green.path().join("unit.json"), r#"{"stage":"unit-tests","verdict":"green"}"#).unwrap();
        let none = io::red_test_phases(green.path());
        assert!(none.is_empty());
        let why = blocks_attribution(&[], &none, &[], Some(green.path())).expect("blocks");
        assert!(why.contains(&green.path().display().to_string()));
        assert_eq!(blocks_attribution(&["test-a.sh".into()], &none, &[], None), None);
    }

    #[test_id(ttj3ecv)]
    #[test]
    fn a_red_fence_or_lint_result_is_a_red_the_round_can_attribute() {
        let dir = testkit::TempDir::new("batcher-cut-red-filter");
        std::fs::write(dir.path().join("fence-x.result"), "red 1 1 - - - 1\n").unwrap();
        let red = vm::red_filters(dir.path());
        assert_eq!(red, ["fence-x"]);
        assert_eq!(blocks_attribution(&[], &[], &red, Some(dir.path())), None);
        let why = blocks_attribution(&[], &[], &[], Some(dir.path())).expect("blocks");
        assert!(why.contains("no red lint or fence"), "{why}");
    }

    #[test_id(tjoskbv)]
    #[test]
    fn a_red_round_image_result_does_not_block_attribution() {
        let dir = testkit::TempDir::new("batcher-cut-red-image");
        std::fs::write(dir.path().join("fence-round-image.result"), "red 1 0 - - - 1\n").unwrap();
        let red = vm::red_filters(dir.path());
        assert_eq!(red, ["fence-round-image"]);
        assert_eq!(blocks_attribution(&[], &[], &red, Some(dir.path())), None);
    }

    #[test_id(tt91g2o)]
    #[test]
    fn named_suites_narrow_the_corpus_and_drop_names_the_tree_lacks() {
        let all = vec!["test-a.sh".to_string(), "test-b.sh".to_string()];
        assert_eq!(named_suites(all.clone(), None), all);
        assert_eq!(named_suites(all.clone(), Some("test-b.sh, test-z.sh")), ["test-b.sh"]);
        assert!(named_suites(all, Some("test-z.sh")).is_empty());
    }

    #[test_id(t4id56l)]
    #[test]
    fn a_cut_round_lands_the_config_delta_folded_and_its_fragments_gone() {
        let root = testkit::TempDir::new("batcher-cut-fold");
        let d = root.path().join("repo");
        std::fs::create_dir_all(d.join("spira/config-delta.d")).unwrap();
        let g = |args: &[&str]| testkit::git(&d, args);
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("spira/config-delta.toml"), "# base\n[spira]\nk = 1\n").unwrap();
        let commit = |msg: &str| {
            g(&["add", "-A"]);
            g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]);
            g(&["rev-parse", "HEAD"]).trim().to_string()
        };
        let base = commit("base");
        let mut members = Vec::new();
        for (id, key) in [("sp-a", "spira.x"), ("sp-b", "spira.y")] {
            g(&["checkout", "-q", "-b", id, &base]);
            std::fs::create_dir_all(d.join("spira/config-delta.d")).unwrap();
            std::fs::write(d.join(format!("spira/config-delta.d/{id}.toml")), format!("removed = [\"{key}\"]\n")).unwrap();
            members.push(Member { id: id.into(), tip: commit(id), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![], provisional: false });
        }
        g(&["checkout", "-q", "main"]);
        let env = io::lifecycle_tests_env(root.path());
        let repo = Repo { name: "alpha".into(), path: d.clone(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        let wt = root.path().join("wt");
        let merged = merge_round(&env, &repo, &wt, &base, members, &Default::default()).unwrap();
        assert_eq!(merged.len(), 2);
        let delta = std::fs::read_to_string(wt.join("spira/config-delta.toml")).unwrap();
        assert!(delta.contains("# base") && delta.contains("spira.x") && delta.contains("spira.y"), "{delta}");
        assert!(!wt.join("spira/config-delta.d/sp-a.toml").exists() && !wt.join("spira/config-delta.d/sp-b.toml").exists());
        let status = testkit::git(&wt, &["status", "--porcelain"]);
        assert!(status.trim().is_empty(), "the fold is committed, not left dirty: {status}");
        assert!(testkit::git(&wt, &["log", "-1", "--format=%B"]).contains("Members: sp-a sp-b"));
        let own = vm::RoundCommits::default();
        merge_round(&env, &repo, &wt, &base, merged.clone(), &own).unwrap();
        let head = testkit::git(&wt, &["rev-parse", "HEAD"]).trim().to_string();
        assert_eq!(*own.borrow(), vec![head], "the fold commit's sha is recorded as the round's own");
        merge_round(&env, &repo, &wt, &base, merged.clone(), &Default::default()).unwrap();
        assert_eq!(testkit::git(&wt, &["rev-list", "--count", &format!("{base}..HEAD")]).trim(), "5", "a rebuild folds once from a reset tree: two members, two merges, one fold");
        g(&["-C", wt.to_str().unwrap(), "checkout", "-q", "--detach", &base]);
        assert_eq!(io::fold_config_delta(&env, &wt, &merged).unwrap(), None, "a tree with no fragments gets no commit");
    }

    const ALL: Switches = Switches {
        local: true,
        forge: true,
    };

    #[test_id(tcf0cl4)]
    #[test]
    fn a_rounds_stop_on_one_repo_leaves_another_cutting() {
        let d = testkit::TempDir::new("batcher-rounds-stop");
        assert!(
            rounds_stop_note(d.path(), "spira").is_none(),
            "no stop, no note: the matcher can be silent"
        );
        let lanes = |l: &str| spira_config::scoped_stop::parse_lanes(l).unwrap();
        spira_config::scoped_stop::pause_repo(
            d.path(),
            "spira",
            &lanes("rounds"),
            "concierge",
            "hand fix",
            "fix lands",
            1,
        )
        .unwrap();
        assert!(rounds_stop_note(d.path(), "spira")
            .unwrap()
            .contains("stopped on purpose by concierge"));
        assert!(rounds_stop_note(d.path(), "cubby").is_none());
        spira_config::scoped_stop::unpause_repo(d.path(), "spira", &lanes("claims")).unwrap();
        assert!(
            rounds_stop_note(d.path(), "spira").is_some(),
            "lifting another lane keeps rounds stopped"
        );
    }

    #[test_id(trf9puc)]
    #[test]
    fn a_hand_landings_cut_hold_stops_the_cut_of_its_repo_only() {
        let d = testkit::TempDir::new("batcher-cut-hold");
        assert!(cut_hold_note(d.path(), "spira").is_none(), "no hold, no note: the matcher can be silent");
        let far = u64::MAX;
        let hold = spira_config::cut_hold::CutHold {
            owner: "hand-land sp-x".into(),
            reason: "landing out of turn".into(),
            pid: std::process::id(),
            since: 1,
            until: far,
        };
        spira_config::cut_hold::place(d.path(), "spira", &hold).unwrap();
        let note = cut_hold_note(d.path(), "spira").unwrap();
        assert!(note.contains("hand-land sp-x") && note.contains("no cut"), "{note}");
        assert!(cut_hold_note(d.path(), "cubby").is_none());
        spira_config::cut_hold::lift(d.path(), "spira", std::process::id());
        assert!(cut_hold_note(d.path(), "spira").is_none());
    }

    #[test_id(tiugops)]
    #[test]
    fn a_pass_drives_each_repo_by_its_own_land_mode_and_never_mixes() {
        let repos: Vec<(String, String)> = [
            ("spira", "queue.local"),
            ("deck", "queue"),
            ("poke", "queue"),
            ("other", "push"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let run = |pass, enabled: Switches| {
            let mut seen = Vec::new();
            let failed = drive_pass(&repos, "spira", pass, enabled, |n, s| {
                seen.push(format!(
                    "{n}:{}",
                    if *s == Step::Verdict {
                        "verdict"
                    } else {
                        "cut"
                    }
                ));
                Ok(())
            });
            assert!(failed.is_empty());
            seen
        };
        assert_eq!(
            run(Pass::Forge, ALL),
            ["deck:verdict", "deck:cut", "poke:verdict", "poke:cut"]
        );
        assert_eq!(run(Pass::Local, ALL), ["spira:cut"]);
        assert_eq!(
            run(Pass::All, ALL),
            [
                "spira:cut",
                "deck:verdict",
                "deck:cut",
                "poke:verdict",
                "poke:cut"
            ]
        );
    }

    #[test_id(t471ytd)]
    #[test]
    fn the_local_hold_never_holds_a_forge_batch_and_the_forge_switch_holds_only_forge() {
        let repos = vec![
            ("spira".to_string(), "queue.local".to_string()),
            ("deck".to_string(), "queue".to_string()),
        ];
        let seen = |on| {
            let mut seen = Vec::new();
            drive_pass(&repos, "spira", Pass::All, on, |n, _| {
                seen.push(n.to_string());
                Ok(())
            });
            seen
        };
        assert_eq!(
            seen(Switches {
                local: false,
                forge: true
            }),
            ["deck", "deck"]
        );
        assert_eq!(
            seen(Switches {
                local: true,
                forge: false
            }),
            ["spira"]
        );
        assert!(seen(Switches {
            local: false,
            forge: false
        })
        .is_empty());
    }

    #[test_id(tg4g0sn)]
    #[test]
    fn the_local_hold_holds_only_the_home_repo_and_a_sibling_queue_local_repo_is_still_cut() {
        let repos = vec![
            ("spira".to_string(), "queue.local".to_string()),
            ("cubby".to_string(), "queue.local".to_string()),
        ];
        let seen = |on, pass| {
            let mut seen = Vec::new();
            drive_pass(&repos, "spira", pass, on, |n, _| {
                seen.push(n.to_string());
                Ok(())
            });
            seen
        };
        assert_eq!(
            seen(
                Switches {
                    local: false,
                    forge: true
                },
                Pass::Local
            ),
            ["cubby"]
        );
        assert_eq!(
            seen(
                Switches {
                    local: false,
                    forge: false
                },
                Pass::All
            ),
            ["cubby"]
        );
        assert_eq!(
            seen(
                Switches {
                    local: true,
                    forge: true
                },
                Pass::Local
            ),
            ["spira", "cubby"]
        );
    }

    #[test_id(tvvanlw)]
    #[test]
    fn one_repos_failure_does_not_stop_the_others_and_a_failed_verdict_still_cuts() {
        let repos = vec![
            ("deck".to_string(), "queue".to_string()),
            ("poke".to_string(), "queue".to_string()),
        ];
        let mut seen = Vec::new();
        let failed = drive_pass(&repos, "spira", Pass::Forge, ALL, |n, s| {
            seen.push(format!(
                "{n}:{}",
                if *s == Step::Verdict {
                    "verdict"
                } else {
                    "cut"
                }
            ));
            if n == "deck" {
                Err("boom".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(
            seen,
            ["deck:verdict", "deck:cut", "poke:verdict", "poke:cut"]
        );
        assert_eq!(failed, ["deck"]);
    }
}

#[cfg(test)]
mod e2e;

#[cfg(test)]
mod base_conflict_handling {
    use super::*;

    use std::fs;

    #[test_id(ttx2ww0)]
    #[test]
    fn a_lock_waiter_can_name_the_batcher_holding_the_queue_lock() {
        let d = testkit::TempDir::new("batcher-cut-lock-holder");
        let e = io::lifecycle_tests_env(&d);
        let held = io::try_lock(&e, "deck").unwrap().unwrap();
        let text = fs::read_to_string(d.join("queue/deck/lock")).unwrap();
        assert!(
            text.contains(&format!("pid={}", std::process::id())) && text.contains("verb=batcher"),
            "{text}"
        );
        drop(held);
        assert_eq!(fs::read_to_string(d.join("queue/deck/lock")).unwrap(), "");
    }
}

#[cfg(test)]
mod vtime_tests {
    use super::*;

    #[test_id(txl9nkv)]
    #[test]
    fn utc_stamp_formats_epoch_seconds() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_791_687_211), "2026-10-11T02:53:31Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test_id(tqgljb9)]
    #[test]
    fn now_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, now);
        assert_eq!(got, 1_900_000_000);
    }
}

#[cfg(test)]
mod fork_fence {
    #[test_id(tklpe63)]
    #[test]
    fn batcher_cut_starts_no_spira_child_that_is_not_a_named_fork_edge() {
        let got =
            forge::fork_scan::scan(&testkit::manifest_dir().join("src"));
        assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
    }
}

#[cfg(test)]
mod cut_tests {
    use super::*;

    #[test_id(ta0ngxn)]
    #[test]
    fn registry_row_beads_names_each_bead_once_and_a_failing_registry_says_what_it_printed() {
        let d = testkit::TempDir::new("batcher-cut-registry-beads");
        let mut env = io::lifecycle_tests_env(&d);
        let bin = d.join("test-registry");
        env.test_registry_bin = Some(bin.clone());
        let body = r#"{"ok":true,"rows":[{"test":"suite:a.sh","bead":"sp-two"},{"test":"unit:x t::y","bead":"sp-one"},{"test":"suite:b.sh","bead":"sp-two"},{"test":"suite:c.sh","bead":"-"}]}"#;
        testkit::write_exe(&bin, &format!("#!/bin/sh\n[ \"$1 $2\" = \"list --json\" ] || exit 9\necho '{body}'\n"));
        assert_eq!(io::registry_row_beads(&env).unwrap(), vec!["sp-one".to_string(), "sp-two".to_string()]);
        testkit::write_exe(&bin, "#!/bin/sh\necho \"registry locked $*\" >&2\nexit 3\n");
        let err = io::registry_row_beads(&env).unwrap_err();
        assert!(err.contains("registry locked list --json") && err.contains("exited 3"), "{err}");
    }

    #[test_id(tpftdo6)]
    #[test]
    fn clear_landed_rows_passes_each_bead_and_returns_the_cleared_tests_or_the_registrys_words() {
        let d = testkit::TempDir::new("batcher-cut-clear-landed-rows");
        let mut env = io::lifecycle_tests_env(&d);
        let bin = d.join("test-registry");
        env.test_registry_bin = Some(bin.clone());
        testkit::write_exe(&bin, "#!/bin/sh\necho \"$*\" > \"$0.args\"\necho 'suite:a.sh'\necho 'unit:x t::y'\n");
        let beads = vec!["sp-one".to_string(), "sp-two".to_string()];
        assert_eq!(io::clear_landed_rows(&env, &beads).unwrap(), vec!["suite:a.sh".to_string(), "unit:x t::y".to_string()]);
        assert_eq!(std::fs::read_to_string(d.join("test-registry.args")).unwrap().trim(), "clear-landed --bead sp-one --bead sp-two --actor batcher");
        testkit::write_exe(&bin, "#!/bin/sh\necho \"registry busy\" >&2\nexit 5\n");
        let err = io::clear_landed_rows(&env, &beads).unwrap_err();
        assert!(err.contains("exited 5") && err.contains("registry busy"), "{err}");
    }

    #[test_id(t2va2j5)]
    #[test]
    fn a_failing_registry_cuts_no_round_and_says_what_it_printed() {
        let d = testkit::TempDir::new("batcher-cut-deny-fail");
        let env = io::lifecycle_tests_env(&d);
        let bin = d.join("test-registry");
        testkit::write_exe(&bin, "#!/bin/sh\necho \"registry locked $*\" >&2\nexit 3\n");
        let err = snapshot_from(io::deny_list_from(bin.to_str().unwrap(), &env, 77), 77).unwrap_err();
        assert!(err.msg.contains("registry locked deny-list --at 77") && err.msg.contains("exited 3"), "{}", err.msg);
        assert!(snapshot_from(io::deny_list_from(d.join("nonexistent/test-registry").to_str().unwrap(), &env, 77), 77).is_err());
    }

    #[test_id(twdadym)]
    #[test]
    fn a_cut_resolves_the_list_at_its_start_and_every_round_row_names_it() {
        let d = testkit::TempDir::new("batcher-cut-deny-row");
        let mut env = io::lifecycle_tests_env(&d);
        let bin = d.join("test-registry");
        testkit::write_exe(&bin, "#!/bin/sh\nprintf '# test-registry deny-list at=%s rows=1\\ndisabled\\ttest:t0aaaaa\\tsp-x\\t-\\n' \"$3\"\n");
        let deny = snapshot_from(io::deny_list_from(bin.to_str().unwrap(), &env, 77), 77).unwrap();
        assert_eq!(deny.at, 77);
        assert_eq!(deny.text, "# test-registry deny-list at=77 rows=1\ndisabled\ttest:t0aaaaa\tsp-x\t-\n");
        let log = d.join("tsd.log");
        env.tsd_bin = Some(d.join("tsd-write"));
        testkit::write_exe(env.tsd_bin.as_ref().unwrap(), &format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()));
        append_round(&env, Some(&deny), &[("repo", "fx".to_string()), ("verdict", "emptied".to_string())]);
        let row = std::fs::read_to_string(&log).unwrap();
        assert!(row.contains(&format!("--field-str deny_list={}", deny.label())) && row.contains("--field-str deny_rows=1"), "{row}");
        append_round(&env, None, &[("repo", "fx".to_string())]);
        assert!(!std::fs::read_to_string(&log).unwrap().lines().nth(1).unwrap().contains("deny_list"));
    }

    fn flip_fixture(d: &Path) -> (Env, PathBuf, PathBuf, PathBuf) {
        let env = io::lifecycle_tests_env(d);
        let results = d.join("results");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::write(results.join("deny-list"), "# test-registry deny-list at=77 rows=1\ndisabled\ttest:t0old00\tsp-x\t900\n").unwrap();
        let calls = d.join("registry-calls");
        let bin = d.join("test-registry");
        testkit::write_exe(&bin, &format!("#!/bin/sh\necho \"$@\" >> {}\nprintf '{{\"ok\":true,\"row\":{{\"test\":\"%s\",\"state\":\"disabled\",\"bead\":\"%s\",\"until\":5000}}}}' \"$4\" \"$6\"\n", calls.display()));
        (env, results, bin, calls)
    }

    #[test_id(t5qcncs)]
    #[test]
    fn a_flip_files_one_bead_per_test_then_writes_one_row_naming_it_as_the_batcher_and_extends_the_deny_list() {
        let d = testkit::TempDir::new("batcher-cut-flip-rows");
        let (env, results, bin, calls) = flip_fixture(&d);
        let wt = d.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        testkit::git(&wt, &["init", "-q", "-b", "main"]);
        testkit::git(&wt, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let head = io::head_of(&wt).unwrap();
        let mut filed = vec![];
        let mut n = 0;
        let rows = io::disable_flips_with(
            &mut |id| {
                filed.push(id.to_string());
                n += 1;
                Ok(format!("sp-new{n}"))
            },
            &mut io::through_registry(bin.to_str().unwrap(), &env),
            &results,
            "spira/batcher-attr/fx-1",
            &["test:t0aaaaa".to_string(), "suite:test-x.sh".to_string()],
        )
        .unwrap();
        assert_eq!(filed, ["test:t0aaaaa", "suite:test-x.sh"]);
        let calls = std::fs::read_to_string(&calls).unwrap();
        let lines: Vec<&str> = calls.lines().collect();
        assert_eq!(lines.len(), 2, "{calls}");
        assert!(lines[0].starts_with("disable --json --test test:t0aaaaa --bead sp-new1 --reason test:t0aaaaa flipped in spira/batcher-attr/fx-1 ("), "{}", lines[0]);
        assert!(lines[0].ends_with("--actor batcher") && !lines[0].contains("--until"), "{}", lines[0]);
        assert!(lines[1].contains("--bead sp-new2"), "{}", lines[1]);
        assert_eq!(rows.iter().map(|r| r.bead.as_str()).collect::<Vec<_>>(), ["sp-new1", "sp-new2"]);
        let list = vm::DenySnapshot::from_pass(&results).unwrap();
        assert_eq!(
            list.text,
            "# test-registry deny-list at=77 rows=3\ndisabled\ttest:t0old00\tsp-x\t900\ndisabled\ttest:t0aaaaa\tsp-new1\t5000\ndisabled\tsuite:test-x.sh\tsp-new2\t5000\n"
        );
        assert_eq!(io::head_of(&wt).unwrap(), head, "no commit is made in the round tree");
    }

    #[test_id(tco3ojg)]
    #[test]
    fn a_flip_whose_bead_cannot_be_filed_writes_no_row_and_one_whose_row_is_refused_leaves_the_list_alone() {
        let d = testkit::TempDir::new("batcher-cut-flip-fail");
        let (env, results, bin, calls) = flip_fixture(&d);
        let before = std::fs::read_to_string(results.join("deny-list")).unwrap();
        let ids = ["test:t0aaaaa".to_string()];
        let err = io::disable_flips_with(&mut |_| Err("bead store down".into()), &mut io::through_registry(bin.to_str().unwrap(), &env), &results, "b", &ids).unwrap_err();
        assert!(err.contains("bead store down") && err.contains("test:t0aaaaa"), "{err}");
        assert!(!calls.exists());
        testkit::write_exe(d.join("refuse"), "#!/bin/sh\necho locked\nexit 3\n");
        let err = io::disable_flips_with(&mut |_| Ok("sp-n".into()), &mut io::through_registry(d.join("refuse").to_str().unwrap(), &env), &results, "b", &ids).unwrap_err();
        assert!(err.contains("locked") && err.contains("sp-n"), "{err}");
        assert_eq!(std::fs::read_to_string(results.join("deny-list")).unwrap(), before);
    }

    #[test_id(trfipfu)]
    #[test]
    fn a_flip_row_reaches_the_deny_list_the_rounds_next_certify_reads_and_a_missing_list_is_an_error() {
        let d = testkit::TempDir::new("batcher-cut-flip-kept");
        let (env, results, bin, _) = flip_fixture(&d);
        io::disable_flips_with(&mut |_| Ok("sp-new1".into()), &mut io::through_registry(bin.to_str().unwrap(), &env), &results, "b", &["test:t0aaaaa".to_string()]).unwrap();
        let run = d.join("run");
        io::keep_attribution_deny_list(&run, "r-1", &results).unwrap();
        let kept = std::fs::read_to_string(io::round_results_dir(&run, "r-1").join("deny-list")).unwrap();
        assert!(kept.contains("rows=2") && kept.contains("disabled\ttest:t0aaaaa\tsp-new1\t5000\n"), "{kept}");
        assert!(io::keep_attribution_deny_list(&run, "r-1", &d.join("nowhere")).is_err());
    }

    fn kept_row() -> io::KeptRow {
        io::KeptRow {
            test: "suite:test-x.sh".into(),
            owner: "sp-own".into(),
            old_bead: "sp-own".into(),
            round_branch: "spira/batcher-attr/fx-1".into(),
            head: "h1".into(),
            base: "b1".into(),
            merged_lines: vec!["not ok 7".into()],
            base_lines: vec![],
        }
    }

    #[test_id(ttvcifq)]
    #[test]
    fn a_kept_row_files_one_fix_bead_repoints_the_row_at_it_and_puts_the_row_back_in_the_deny_list() {
        let d = testkit::TempDir::new("batcher-cut-keep-row");
        let (env, results, bin, calls) = flip_fixture(&d);
        let mut filed = vec![];
        let row = io::keep_row_with(
            &mut |k| {
                filed.push(k.test.clone());
                Ok("sp-fix".into())
            },
            &mut io::through_registry(bin.to_str().unwrap(), &env),
            &results,
            &kept_row(),
        )
        .unwrap();
        assert_eq!((filed.as_slice(), row.bead.as_str()), (&["suite:test-x.sh".to_string()][..], "sp-fix"));
        let calls = std::fs::read_to_string(&calls).unwrap();
        assert!(
            calls.starts_with("disable --json --test suite:test-x.sh --bead sp-fix --reason kept: suite:test-x.sh red on spira/batcher-attr/fx-1 and on the base b1; sp-own's landing did not clear it; re-pointed from sp-own to sp-fix"),
            "{calls}"
        );
        assert_eq!(
            vm::DenySnapshot::from_pass(&results).unwrap().text,
            "# test-registry deny-list at=77 rows=2\ndisabled\ttest:t0old00\tsp-x\t900\ndisabled\tsuite:test-x.sh\tsp-fix\t5000\n"
        );
    }

    #[test_id(tr35es3)]
    #[test]
    fn a_kept_row_whose_bead_cannot_be_filed_touches_neither_the_registry_nor_the_deny_list() {
        let d = testkit::TempDir::new("batcher-cut-keep-row-fail");
        let (env, results, bin, calls) = flip_fixture(&d);
        let before = std::fs::read_to_string(results.join("deny-list")).unwrap();
        let err = io::keep_row_with(&mut |_| Err("bead store down".into()), &mut io::through_registry(bin.to_str().unwrap(), &env), &results, &kept_row()).unwrap_err();
        assert!(err.contains("bead store down"), "{err}");
        assert!(!calls.exists());
        assert_eq!(std::fs::read_to_string(results.join("deny-list")).unwrap(), before);
    }

    #[test_id(tt2hpr6)]
    #[test]
    fn an_ejected_owners_row_goes_back_into_the_deny_list_with_no_expiry_written_as_a_dash() {
        let d = testkit::TempDir::new("batcher-cut-restore-row");
        let (_, results, _, _) = flip_fixture(&d);
        let row = vm::LiftedRow { state: "disabled".into(), test: "suite:test-y.sh".into(), bead: "sp-own".into(), until: -1, claimed_by: None };
        io::add_row_to_pass(&results, &row).unwrap();
        assert_eq!(
            vm::DenySnapshot::from_pass(&results).unwrap().text,
            "# test-registry deny-list at=77 rows=2\ndisabled\ttest:t0old00\tsp-x\t900\ndisabled\tsuite:test-y.sh\tsp-own\t-\n"
        );
    }

    fn lifted_pair() -> io::LiftedRows {
        let row = |t: &str, b: &str| vm::LiftedRow { state: "disabled".into(), test: t.into(), bead: b.into(), until: 900, claimed_by: None };
        io::LiftedRows::new(vec![row("suite:test-x.sh", "sp-own"), row("test:t0yyyyy", "sp-own"), row("suite:test-z.sh", "sp-other")])
    }

    #[test_id(tc0uo4u)]
    #[test]
    fn a_row_is_kept_once_per_round_and_the_second_call_files_nothing() {
        let mut rows = lifted_pair();
        let mut filed = vec![];
        let first = rows.keep("suite:test-x.sh", |old| {
            filed.push(old.to_string());
            Ok("sp-fix".into())
        });
        let second = rows.keep("suite:test-x.sh", |old| {
            filed.push(old.to_string());
            Ok("sp-other-fix".into())
        });
        assert_eq!((first, second), (Ok("sp-fix".to_string()), Ok("sp-fix".to_string())));
        assert_eq!(filed, ["sp-own"], "the old bead is the row's, and only one filing");
        assert!(rows.keep("suite:nope.sh", |_| Ok("x".into())).is_err());
        assert_eq!(rows.standing().iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), ["test:t0yyyyy", "suite:test-z.sh"]);
        let mut failing = lifted_pair();
        assert!(failing.keep("suite:test-x.sh", |_| Err("down".into())).is_err());
        assert_eq!(failing.standing().len(), 3, "a failed keep leaves the row standing and retryable");
    }

    #[test_id(tzrxx82)]
    #[test]
    fn an_ejected_owners_rows_go_back_into_the_deny_list_once_and_a_kept_row_is_left_alone() {
        let d = testkit::TempDir::new("batcher-cut-restore-owner");
        let (_, results, _, _) = flip_fixture(&d);
        let mut rows = lifted_pair();
        rows.keep("suite:test-x.sh", |_| Ok("sp-fix".into())).unwrap();
        rows.restore_rows_of("sp-own", &results).unwrap();
        rows.restore_rows_of("sp-own", &results).unwrap();
        assert_eq!(
            vm::DenySnapshot::from_pass(&results).unwrap().text,
            "# test-registry deny-list at=77 rows=2\ndisabled\ttest:t0old00\tsp-x\t900\ndisabled\ttest:t0yyyyy\tsp-own\t900\n"
        );
        assert_eq!(rows.standing().iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), ["suite:test-z.sh"]);
    }

    #[test_id(t8irari)]
    #[test]
    fn the_round_record_names_the_rows_the_round_lifted() {
        let d = testkit::TempDir::new("batcher-cut-lifted-record");
        let mut env = io::lifecycle_tests_env(&d);
        let log = d.join("tsd.log");
        env.tsd_bin = Some(d.join("tsd-write"));
        testkit::write_exe(env.tsd_bin.as_ref().unwrap(), &format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()));
        let all = vm::DenySnapshot { text: "# test-registry deny-list at=5 rows=1\ndisabled\tsuite:a.sh\tsp-m\t9\n".into(), at: 5, lifted: vec![], claimed: vec![] };
        let (deny, _) = all.lifting(&["sp-m".to_string()], &[]);
        append_round(&env, Some(&deny), &[("repo", "fx".to_string())]);
        assert!(std::fs::read_to_string(&log).unwrap().contains("--field-str deny_lifted=suite:a.sh"));
    }

    fn lint_red_attribution(d: &Path, with_deny_list: bool) -> (Result<String, String>, PathBuf) {
        lint_red_attribution_of(
            d,
            with_deny_list,
            &[("sp-8br220", "pilgrim.rs"), ("sp-x", "other.rs")],
            &[("r", "pilgrim.rs")],
            None,
            None,
        )
    }

    fn lint_red_attribution_of(
        d: &Path,
        with_deny_list: bool,
        members: &[(&str, &str)],
        findings: &[(&str, &str)],
        suites: Option<&str>,
        batch: Option<&str>,
    ) -> (Result<String, String>, PathBuf) {
        let repo_dir = d.join("repo");
        std::fs::create_dir_all(repo_dir.join("spira")).unwrap();
        testkit::git(&repo_dir, &["init", "-q", "-b", "main"]);
        std::fs::write(repo_dir.join("spira/test-a.sh"), "").unwrap();
        testkit::git(&repo_dir, &["add", "-A"]);
        testkit::git(&repo_dir, &["commit", "-q", "-m", "base"]);
        let start = testkit::git(&repo_dir, &["rev-parse", "HEAD"]);
        let mut tips = vec![];
        for (id, file) in members {
            testkit::git(&repo_dir, &["checkout", "-q", "-b", &format!("spira/{id}"), &start]);
            std::fs::write(repo_dir.join(file), "x\n").unwrap();
            testkit::git(&repo_dir, &["add", "-A"]);
            testkit::git(&repo_dir, &["commit", "-q", "-m", id]);
            tips.push(testkit::git(&repo_dir, &["rev-parse", "HEAD"]));
        }
        testkit::git(&repo_dir, &["checkout", "-q", "main"]);
        let wt = d.join("round-wt");
        testkit::git(&repo_dir, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), &start]);
        for tip in &tips {
            testkit::git(&wt, &["merge", "-q", "--no-ff", "--no-edit", tip]);
        }
        let pass = d.join("rounds/b-1.results");
        std::fs::create_dir_all(&pass).unwrap();
        for f in ["disk-usage", "fence-times", "round-phase", "round-script.log", "run.events", "runner.out", "unit-build", "vm-batch.err", "vm-batch.out"] {
            std::fs::write(pass.join(f), "").unwrap();
        }
        std::fs::write(pass.join("spira-lint.result"), "red\n").unwrap();
        std::fs::write(pass.join("spira-lint.out"), "r: plain finding text\n").unwrap();
        let records: String = findings
            .iter()
            .map(|(rule, path)| {
                let f = spira_lint::Finding { rule: Box::leak(rule.to_string().into_boxed_str()), path: path.to_string(), line: Some(1), message: "bad".into() };
                format!("{}\n", spira_lint::record::FindingRecord::of(&f, Some(("spira-lint/src/rules/r.rs", None))).to_line())
            })
            .collect();
        std::fs::write(pass.join(vm::LINT_FINDINGS_FILE), records).unwrap();
        if with_deny_list {
            std::fs::write(pass.join("deny-list"), "# test-registry deny-list at=7 rows=0\n").unwrap();
        }
        let mut env = io::lifecycle_tests_env(d);
        let calls = d.join("round-vm-calls");
        testkit::write_exe(&env.round_vm, &format!("#!/bin/sh\necho \"$@\" >> {}\nexit 2\n", calls.display()));
        let queue_calls = d.join("queue-calls");
        testkit::write_exe(&env.queue_bin, &format!("#!/bin/sh\necho \"CALL $*\" >> {}\ncat >> {0}\nexit 0\n", queue_calls.display()));
        env.maxpar = 1;
        let repo = io::Repo { name: "fx".into(), path: repo_dir, base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        let o = Opts {
            cmd: "attribute".into(),
            repo: "fx".into(),
            run: Some(d.to_path_buf()),
            db: None,
            home: Some(d.to_path_buf()),
            round_vm: None,
            suites: suites.map(str::to_string),
            members: None,
            evidence: None,
            reds: None,
            after_pass: Some(pass),
            record: None,
            wt: Some(wt.clone()),
            base: Some(start.clone()),
            batch: batch.map(str::to_string),
            by: None,
            reason: None,
            all: false,
            held_since: None, out: None,
            confirmed: false,
        };
        let given: Vec<(String, String)> = members.iter().zip(&tips).map(|((id, _), tip)| (id.to_string(), tip.clone())).collect();
        (attribute_in(&o, env, repo, wt, start, given), calls)
    }

    #[test_id(tzxvqnn)]
    #[test]
    fn a_lint_red_certify_pass_with_its_deny_list_ejects_the_member_it_names_without_running_the_corpus() {
        let d = testkit::TempDir::new("batcher-cut-attr-lint");
        let (line, calls) = lint_red_attribution(&d, true);
        let line = line.unwrap();
        assert!(line.contains("\"end\":\"land\"") && line.contains("[[\"sp-x\"") && !line.contains("sp-8br220"), "{line}");
        assert!(!calls.exists(), "the corpus was requested: {:?}", std::fs::read_to_string(&calls));
    }

    #[test_id(t47imqp)]
    #[test]
    fn a_lint_red_named_to_the_pass_ejects_every_member_it_names_with_all_their_lines() {
        let d = testkit::TempDir::new("batcher-cut-attr-lint-suites");
        let out = [("r", "a.rs"), ("r", "b.rs"), ("r2", "a.rs")];
        let (line, calls) = lint_red_attribution_of(
            &d,
            true,
            &[("sp-a", "a.rs"), ("sp-b", "b.rs"), ("sp-c", "c.rs")],
            &out,
            Some("spira-lint"),
            Some("b-1"),
        );
        let line = line.unwrap();
        assert!(line.contains("\"end\":\"land\"") && line.contains("[[\"sp-c\"") && !line.contains("sp-a") && !line.contains("sp-b"), "{line}");
        assert!(!calls.exists(), "the corpus was requested");
        let q = std::fs::read_to_string(d.join("queue-calls")).unwrap();
        let ejects: Vec<&str> = q.split("CALL ").filter(|c| c.contains("eject")).collect();
        assert_eq!(ejects.len(), 2, "{q:?}");
        let a = ejects.iter().find(|l| l.contains("sp-a")).unwrap();
        assert!(a.contains("r: a.rs:1: bad") && a.contains("r2: a.rs:1: bad"), "{a:?}");
        let b = ejects.iter().find(|l| l.contains("sp-b")).unwrap();
        assert!(b.contains("r: b.rs:1: bad") && !b.contains("a.rs"), "{b:?}");
    }

    #[test_id(tvo99ct)]
    #[test]
    fn a_lint_red_line_no_member_owns_blocks_the_round_and_quotes_the_line() {
        let d = testkit::TempDir::new("batcher-cut-attr-lint-base");
        let out = [("r", "a.rs"), ("r", "h.rs")];
        let (line, _) = lint_red_attribution_of(
            &d,
            true,
            &[("sp-a", "a.rs"), ("sp-b", "b.rs"), ("sp-c", "c.rs")],
            &out,
            Some("spira-lint"),
            None,
        );
        let line = line.unwrap();
        assert!(line.contains("\"end\":\"blocked\"") && line.contains("h.rs:1"), "{line}");
    }

    #[test_id(t68mpx7)]
    #[test]
    fn a_pass_without_its_deny_list_still_refuses_to_attribute() {
        let d = testkit::TempDir::new("batcher-cut-attr-nolist");
        let (line, _) = lint_red_attribution(&d, false);
        assert!(line.unwrap_err().contains("holds no deny list"));
    }

    #[test_id(tsbbqc5)]
    #[test]
    fn a_cut_on_an_ec2_only_repo_never_calls_round_vm_refresh() {
        let d = testkit::TempDir::new("batcher-cut-no-refresh");
        let repo_dir = d.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        testkit::git(&repo_dir, &["init", "-q", "-b", "main"]);
        testkit::git(&repo_dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let mut env = io::lifecycle_tests_env(&d);
        let calls = d.join("round-vm-calls");
        env.round_vm = d.join("round-vm-stub");
        testkit::write_exe(
            &env.round_vm,
            &format!("#!/bin/sh\necho \"$@\" >> {}\nexit 2\n", calls.display()),
        );
        let repo = io::Repo {
            name: "fx".into(),
            path: repo_dir,
            base: "main".into(),
            forge: PathBuf::new(),
            land: io::Land::Local,
        };
        let _ = cut_new_round_inner(&env, &repo, &[], &TriggerReason::MainRed);
        let got = std::fs::read_to_string(&calls).unwrap_or_default();
        assert_eq!(got, "", "the cut called round-vm: {got}");
    }
}
