//! The impure half: git, testenv-batch, the forge and the bead store. Every function here
//! either shells out to a subprocess or touches the filesystem; nothing in `core` does either,
//! so every decision the batcher makes is a fixture `core`'s replay tests can drive without a
//! container, git or the wall clock.
//!
//! Where an operation already has a tested, correct shell implementation (a bead
//! reopen, the priority sort), this seam shells out to lib.sh rather than re-deriving the
//! same bd/event-log side effects a second time in Rust.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::pace::{CutError, CutKind};
use batcher::core::{Member, MergeResult};
use lifecycle::bead::{certifies_nothing, BeadState};

/// Where a repo's landed branch goes — `repo_land`'s `queue`/`queue.forge` alias normalizes
/// to `Forge`; `queue.local` is `Local`. Carried on `Repo` so a caller never has to re-derive
/// it from the base ref's own spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Land {
    Forge,
    Local,
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
    pub forge: PathBuf,
    pub land: Land,
}

/// What the round VM hypervisor can hold, in MiB; a zero host or VM size is an unknown one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hypervisor {
    pub host_mib: u64,
    pub round_vm_mib: u64,
    pub runner_mib: u64,
}

impl Hypervisor {
    /// A staged round starts only beside the certifying round and one CI runner, so both VMs
    /// and the runner must fit; an unknown size never fits.
    pub fn fits_a_staged_round(&self) -> bool {
        self.host_mib > 0
            && self.round_vm_mib > 0
            && self
                .round_vm_mib
                .saturating_mul(2)
                .saturating_add(self.runner_mib)
                <= self.host_mib
    }
}

#[derive(Clone, Debug)]
pub struct Env {
    /// The lifecycle rows one repo step reads: fetched once by `lc_rows`, dropped by `lc_forget`.
    pub lc_rows: std::sync::Arc<std::sync::Mutex<Option<Vec<serde_json::Value>>>>,
    pub home: PathBuf,
    pub run: PathBuf,
    pub queue_dir: PathBuf,
    /// `SPIRA_FORGE` — the forge seam `find_repo` hands to `Repo.forge` (bare name on the
    /// launcher's PATH, sp-gypjk; resolved once, here, not re-read per repo).
    pub forge: PathBuf,
    pub tsd_bin: Option<PathBuf>,
    pub round_vm: PathBuf,
    /// The `queue` program (by name on the launcher's PATH; a unit test hands in a stub): the `round` verbs.
    pub queue_bin: PathBuf,
    /// The `rebase-stale` program (by name on the launcher's PATH).
    pub rebase_stale_bin: PathBuf,
    /// The `testenv` program (by name on the launcher's PATH): a forge repo's own declared tiers, run locally.
    pub testenv_bin: PathBuf,
    /// The `gate` program, which runs one fence of a subset tree (`gate fence-run`).
    pub gate_bin: PathBuf,
    /// The release's `test-registry` program (None when it ships none; clearing is then said, not fatal): a landing clears the rows that name its members.
    pub test_registry_bin: Option<PathBuf>,
    /// The round's slot budget for concurrent attribution (DESIGN.md §4.2):
    /// SPIRA_BATCHER_ROUND_SLOTS, else maxpar + 4.
    pub round_slots: Option<u32>,
    pub hypervisor: Hypervisor,
    /// How often the attribution loop polls the corpus and its reruns.
    pub poll: std::time::Duration,
    /// The Concierge's own proven parallelism (round.sh: SPIRA_BATCH_MAXPAR=16) — set
    /// explicitly on every corpus invocation rather than left for testenv-batch.sh's own
    /// hardware formula, which a bare guest may resolve far below what CI proved green under.
    pub maxpar: u32,
    /// Wall-clock bound on one `round-vm run` (round.sh: `timeout 3600`). cut()/stack_round()
    /// hold the round lock for the duration, so an unbounded run holds it unbounded too;
    /// enforced with `timeout` so a hang cannot outlive this call.
    pub wall_secs: u64,
    /// Seconds an attribution leg's polling wait may see no progress before it gives up.
    pub stall_secs: u64,
    pub git_name: String,
    pub git_email: String,
    pub lc_bin: Option<PathBuf>,
    pub lc_timeout: u64,
    /// `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}` — where the round's tree certificate goes
    /// (gate::cert, queue/DESIGN.md §8 D12), resolved exactly as the gate resolves it.
    #[allow(dead_code)]
    pub verdicts: PathBuf,
    /// How many times `round_land` tries while another queue operation holds the repo's lock.
    pub land_lock_attempts: u32,
    pub land_lock_wait: Duration,
    /// Seconds an open round may go undriven before the rounds loop re-drives it.
    pub orphan_redrive_after: u64,
    pub pause_lift_after: u64,
    /// The Concierge's cut-pause file; a landing clears it unless it starts with `hold:`. `None` is off.
    pub land_unpause: Option<PathBuf>,
    /// `SPIRA_RESULTS_KEEP_DAYS` / `SPIRA_RESULTS_PRUNE_MAX`: how a write of results prunes its folder.
    pub results_retention: spira_config::results_retention::Retention,
    /// The running release's `commit` and `repo` (its MANIFEST); an unreadable release is an `Err`, never a default.
    pub commit: Result<String, String>,
    pub repo: Result<String, String>,
}

fn now() -> u64 {
    spira_config::vtime::now_epoch()
}

fn run(cmd: &mut impl spira_config::bounded::Runnable, what: &str) -> Result<String, String> {
    let o = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let err = err.lines().last().unwrap_or("").trim();
        return Err(format!(
            "{what} exited {}{}",
            o.status.code().unwrap_or(-1),
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

fn run_status(cmd: &mut impl spira_config::bounded::Runnable) -> bool {
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave 4.13"),
/// resolved once, in-process — no `bash -c '. lib.sh; ...'` subprocess at all now: the
/// three separate `repo_land`/`repo_root`/`spira_landref` bash subprocesses `find_repo`
/// used to shell out to, one per lookup, are gone, and so is the one-shot snapshot
/// subprocess that replaced them, since `from_env` resolves
/// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` the same way
/// conf.sh does when this (bare, unit-launched) process's own environment lacks them
/// (sp-z3eyk).
pub fn registry(env: &Env) -> spira_config::repos::Registry {
    spira_config::repos::Registry::from_env(std::env::vars().collect(), &env.home)
}

// ---------------------------------------------------------------------------------------
// lib.sh dispatch — reuse the tested shell functions for bd mutations rather than
// re-deriving their side effects (release_claim, the requeue event, the TSD dual-write).
// ---------------------------------------------------------------------------------------

pub fn lib_call<I, S>(env: &Env, func: &str, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    // FORK-EDGE: needs Rust homes for the lib.sh functions called here (land_subject; bead_reopen: release_claim, the requeue event, the TSD dual-write)
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(r#". "$1" && shift && "$0" "$@""#)
        .arg(func)
        .arg(env.home.join("lib.sh"))
        .args(args);
    // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own release's
    // bin/+spira/ on the CHILD's PATH, never only inherited — see inbox-triage's scar.
    cmd.envs(spira_config::release_env::child_path_env_for_process()?);
    run(&mut cmd, &format!("lib.sh {func}"))
}

/// The queue's own merge subject for `id` — "spira: land <id>", or with " — <title>"
/// appended when the bead has one (land_subject(), lib.sh). Falls back to the untitled form
/// if lib.sh cannot be reached, so a merge is never blocked on this.
fn land_subject(env: &Env, id: &str) -> String {
    lib_call(env, "land_subject", [id]).unwrap_or_else(|_| format!("spira: land {id}"))
}

pub fn bead_reopen(env: &Env, id: &str, cause: &str, note: &str) {
    let _ = lib_call(env, "bead_reopen", [id, cause, note]);
}

/// Tries the mechanical rebase before a member conflicting with the base is handed to an
/// dwarf (sp-oxwvc). Returns the rebase-stale binary's own exit code: 0 (rebased — mechanically or
/// cleanly — and re-certified at the new tip) and 1/2 (a real content conflict or a red
/// gate) all mean the script itself already did everything the caller would otherwise do —
/// certify and note it, or reopen the bead with the conflicting hunk or gate output quoted
/// and bump its requeue count. Only 3 (a branch it could not even attempt: busy, missing,
/// or a scratch-worktree failure) leaves the caller's own fallback bookkeeping to run, the
/// same as if this call had never been made. A code the script never documents (a crash, a
/// missing binary) is folded into 3 for the same reason — silence must fail closed to "an
/// dwarf still sees this", never to "nobody did anything and the round moved on".
pub fn rebase_stale(env: &Env, repo_name: &str, id: &str) -> i32 {
    // batch-job: this runs whatever its caller names, as long as that takes
    // FORK-EDGE: machine leg rebase-stale
    let mut cmd = Command::new(&env.rebase_stale_bin);
    // rebase-stale finds lib.sh through SPIRA_HOME and refuses without it; this pass may have
    // been given --home rather than an exported SPIRA_HOME (conf.sh sets it unexported).
    cmd.arg(id).arg(repo_name).env("SPIRA_HOME", &env.home);
    match cmd.status().ok().and_then(|s| s.code()) {
        Some(0) => 0,
        Some(1) => 1,
        Some(2) => 2,
        _ => 3,
    }
}

// spira-lc: the lifecycle machine, the only source of bead and delivery state here.
// `lc_probe` refuses the cut if the machine is unreachable; the cut/stack calls after it stay
// best-effort and additive (a CAS refusal is reported on stderr and leaves batch_id/version
// unset, never blocking the PR).

pub fn lcq(env: &Env, args: &[&str]) -> Result<String, String> {
    let bin = env
        .lc_bin
        .as_ref()
        .ok_or_else(|| "no spira-lc program".to_string())?;
    let client = spira_store::lc_call::LcClient::for_bin(&bin.to_string_lossy())?
        .within(std::time::Duration::from_secs(env.lc_timeout));
    lc_answer(client.call(args))
}

/// Sets the round's progress record to what `merge` makes of the one stored now, so the keys a
/// pass wrote stay beside the ones `merge` adds.
pub fn batch_progress_merge(env: &Env, batch: &str, merge: impl FnOnce(serde_json::Value) -> serde_json::Value) -> Result<(), String> {
    let bin = env.lc_bin.as_ref().ok_or_else(|| "no spira-lc program".to_string())?;
    let client = spira_store::lc_call::LcClient::for_bin(&bin.to_string_lossy())?.within(std::time::Duration::from_secs(env.lc_timeout));
    batch_progress_merge_via(&client, batch, merge)
}

fn batch_progress_merge_via(client: &spira_store::lc_call::LcClient, batch: &str, merge: impl FnOnce(serde_json::Value) -> serde_json::Value) -> Result<(), String> {
    let shown: serde_json::Value = serde_json::from_str(&lc_answer(client.call(&["show-batch", batch]))?).map_err(|e| format!("show-batch {batch}: {e}"))?;
    let body = merge(shown.get("progress").cloned().unwrap_or(serde_json::Value::Null));
    lc_answer(client.call_stdin(&["batch-progress", batch], &body.to_string())).map(|_| ())
}

/// [`lcq`] for a read the process's store views hold (content, `show-batch`, `list --batches`):
/// answered from them when the process keeps the store's watch, else asked of the store.
pub fn lcq_held(env: &Env, args: &[&str]) -> Result<String, String> {
    match store_view::shared() {
        Some(views) => lc_answer(views.answer(args).map(|r| r.value)),
        None => lcq(env, args),
    }
}

/// `spira-lc show <id>`, from the held lifecycle rows when the process keeps the store's watch: a
/// row carries the `state` and `stack` that `show` does.
fn show_bead(env: &Env, id: &str) -> Result<String, String> {
    match store_view::shared().and_then(|views| views.show(id)) {
        Some(shown) => Ok(shown.map_err(|e| format!("could not say: {e}"))?.value.to_string()),
        None => lcq(env, &["show", id]),
    }
}

/// An answer as the `spira-lc` child's was read: its stdout when it applied; otherwise its exit
/// and the last line it said.
fn lc_answer(
    r: Result<spira_store::lc_call::Reply, spira_store::lc_call::LcError>,
) -> Result<String, String> {
    let r = r.map_err(|e| format!("spira-lc: {e}"))?;
    if r.code != 0 {
        let said = if r.err.trim().is_empty() {
            &r.out
        } else {
            &r.err
        };
        let last = said.lines().last().unwrap_or("").trim();
        return Err(format!(
            "spira-lc exited {}{}",
            r.code,
            if last.is_empty() {
                String::new()
            } else {
                format!(": {last}")
            }
        ));
    }
    Ok(r.out)
}

/// The SUBMITTED and CERTIFIED lifecycle rows, from one `spira-lc list --state` per repo step: the
/// pool and the sifted tips are filters of it. A row outside those states is read by id through
/// `lc_rows_of`, never by listing the store.
pub fn lc_rows(env: &Env) -> Result<Vec<serde_json::Value>, String> {
    if let Some(views) = store_view::shared() {
        let q = store_view::RowQuery { states: &["SUBMITTED", "CERTIFIED"], ..Default::default() };
        if let Some(rows) = views.rows(&q) {
            return rows.map(|r| r.value).map_err(|e| format!("could not say: {e}"));
        }
    }
    let mut held = env.lc_rows.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rows) = held.as_ref() {
        return Ok(rows.clone());
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&lcq(
        env,
        &["list", "--state", "SUBMITTED,CERTIFIED"],
    )?)
    .map_err(|e| format!("spira-lc list: unparsed reply: {e}"))?;
    *held = Some(rows.clone());
    Ok(rows)
}

pub fn lc_rows_of(env: &Env, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(views) = store_view::shared() {
        let q = store_view::RowQuery { ids: Some(ids), ..Default::default() };
        if let Some(rows) = views.rows_held(&q) {
            return rows.map(|r| r.value).map_err(|e| format!("could not say: {e}"));
        }
    }
    serde_json::from_str(&lcq(env, &["list", "--ids", &ids.join(",")])?)
        .map_err(|e| format!("spira-lc list --ids: unparsed reply: {e}"))
}

pub fn lc_forget(env: &Env) {
    *env.lc_rows.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn in_pool_states(r: &serde_json::Value) -> bool {
    matches!(
        r.get("state").and_then(|s| s.as_str()),
        Some("CERTIFIED" | "SUBMITTED")
    )
}

/// Is the machine there to answer? `spira-lc list`, parsed — the same
/// probe the queue crate makes.
pub fn lc_probe(env: &Env) -> Result<(), String> {
    lc_rows(env).map(|_| ())
}

/// `id`'s stack (design stacked-dependents-2026-09-28 §1: `{prereq_bead_id: certified_tip}`)
/// off the lifecycle machine's own bead row — `spira-lc show`. A row with no `stack` column
/// (a bead the machine has never seen) is unstacked; a failed read or an unparseable reply is
/// also read as unstacked — never a hard error a round must refuse over, since `lc_probe`
/// already proved the machine reachable — but is said loudly on stderr.
fn read_stack(env: &Env, id: &str) -> BTreeMap<String, String> {
    let out = match show_bead(env, id) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("batcher: LIFECYCLE: spira-lc show {id} failed (read as unstacked): {e}");
            return BTreeMap::new();
        }
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else {
        eprintln!("batcher: LIFECYCLE: spira-lc show {id}: unparsed reply (read as unstacked)");
        return BTreeMap::new();
    };
    let stack = match v.get("bead").and_then(|b| b.get("stack")) {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null)
        }
        Some(other) => other.clone(),
        None => return BTreeMap::new(),
    };
    let entries: Vec<(String, String)> = stack
        .as_object()
        .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    let ids: Vec<String> = entries.iter().map(|(k, _)| k.clone()).collect();
    let finished = finished_blockers(env, &ids);
    entries.into_iter().filter(|(k, _)| !finished.contains(k)).collect()
}

/// Of `ids`, those finished by the claim rule (`spira_config::edges::blocker_finished`): by their
/// lifecycle row, or with none by their content. A failed read leaves the ids live.
fn finished_blockers(env: &Env, ids: &[String]) -> std::collections::BTreeSet<String> {
    let mut finished = std::collections::BTreeSet::new();
    let Ok(rows) = lc_rows_of(env, ids) else { return finished };
    let mut states: BTreeMap<&str, Option<lifecycle::bead::BeadState>> = BTreeMap::new();
    for r in &rows {
        let Some(id) = r.get("bead_id").and_then(|x| x.as_str()) else { continue };
        let state = r.get("state").and_then(|x| x.as_str()).and_then(lifecycle::bead::BeadState::from_str);
        states.insert(id, state);
        if spira_config::edges::blocker_finished(state, None) {
            finished.insert(id.to_string());
        }
    }
    let rowless: Vec<String> = ids.iter().filter(|i| !states.contains_key(i.as_str())).cloned().collect();
    if let Ok(serde_json::Value::Array(items)) = bd_show(env, &rowless) {
        for it in &items {
            let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
            let text = |k: &str| it.get(k).and_then(|x| x.as_str());
            if spira_config::edges::blocker_finished(None, Some((text("issue_type"), text("status")))) {
                finished.insert(id.to_string());
            }
        }
    }
    finished
}

// ---------------------------------------------------------------------------------------
// The round pool: the lifecycle machine's unheld SUBMITTED and CERTIFIED beads, narrowed to this repo, with
// title/priority from the bead store and express from the lifecycle row.
// ---------------------------------------------------------------------------------------

fn holds_empty(r: &serde_json::Value) -> bool {
    match r.get("holds") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::Array(a)) => a.is_empty(),
        Some(serde_json::Value::String(t)) => serde_json::from_str::<Vec<serde_json::Value>>(t)
            .map(|a| a.is_empty())
            .unwrap_or(false),
        Some(_) => false,
    }
}

/// A pool row: id, tip, certified-at epoch, express, unlanded blockers, and whether it is provisional (SUBMITTED).
type PoolRow = (String, String, u64, bool, Vec<String>, bool);

/// Why a SUBMITTED or CERTIFIED bead is not in the cut.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Cause {
    Held(Vec<String>),
    EjectedRed,
    TipMoved { branch_tip: String },
    Blocked { dep: String },
    StackedOn { prereq: String },
    NoContentRow,
    Refused { kind: String },
    NoSift,
    RoundFull { cap: usize },
    OutsideFeature { root: String },
    GateUnjudged { paths: Vec<String> },
    NotInForce { dep: String },
    InForceUnread { dep: String, error: String },
    Uncertified,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeftOut {
    pub id: String,
    pub tip: String,
    pub cause: Cause,
}

impl LeftOut {
    pub fn tag(&self) -> String {
        match &self.cause {
            Cause::Held(kinds) => format!("held-{}", kinds.join(",")),
            Cause::EjectedRed => "ejected-red".into(),
            Cause::TipMoved { .. } => "tip-moved".into(),
            Cause::Blocked { .. } => "blocked".into(),
            Cause::StackedOn { .. } => "stacked-on-barred".into(),
            Cause::NoContentRow => "no-content-row".into(),
            Cause::Refused { kind } => format!("refused-{kind}"),
            Cause::NoSift => "no-sift".into(),
            Cause::RoundFull { .. } => "round-full".into(),
            Cause::OutsideFeature { .. } => "outside-feature".into(),
            Cause::GateUnjudged { .. } => "gate-unjudged".into(),
            Cause::NotInForce { .. } => "blocked-not-in-force".into(),
            Cause::InForceUnread { .. } => "in-force-unread".into(),
            Cause::Uncertified => "uncertified".into(),
        }
    }

    pub fn line(&self, repo: &str) -> String {
        let (id, tip) = (&self.id, &self.tip);
        let (why, exit) = match &self.cause {
            Cause::Held(kinds) => (
                format!("it is held {}", kinds.join(",")),
                format!("spira-lc unhold {id} {} … readmits it", kinds.join("|")),
            ),
            Cause::EjectedRed => (
                format!("its tip {tip} is a recorded ejected-red tip"),
                format!("a new commit, or queue absolve {id} --evidence-tip {tip} (or an evidence resubmit at it) for a red that was not the tree's fault, readmits it"),
            ),
            Cause::TipMoved { branch_tip } => (
                format!("its certified tip {tip} is no longer the tip of spira/{id}"),
                format!("its gate certifies spira/{id} at {branch_tip}"),
            ),
            Cause::Blocked { dep } => (
                format!("it depends on {dep}, which has not landed"),
                format!("{dep} landing, or the edge removed, readmits it"),
            ),
            Cause::StackedOn { prereq } => (
                format!("it stacks on {prereq}, which is held, ejected at its tip, or itself left out"),
                format!("{prereq} unheld or given a new tip, or the stack edge removed, readmits it"),
            ),
            Cause::NoContentRow => (
                "the content store returned no row for it".into(),
                "a readable content row for the bead readmits it".into(),
            ),
            Cause::Refused { kind } => (
                format!("queue round open refused it ({kind}) at this tip and base"),
                "a new tip, or the base moving, readmits it".into(),
            ),
            Cause::NoSift => (
                format!("its tip {tip} has no sift pass"),
                format!("a sift pass at {tip}, or a new commit that sifts, readmits it"),
            ),
            Cause::RoundFull { cap } => (
                format!("the round is full at {cap} members"),
                "a later round, once this one has landed, takes it".into(),
            ),
            Cause::OutsideFeature { root } => (
                format!("this round is feature {root} and it is not part of it"),
                format!("the {root} round landing lets the next cut take it"),
            ),
            Cause::GateUnjudged { paths } => {
                let shown = paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                let more = if paths.len() > 5 { format!(", +{} more", paths.len() - 5) } else { String::new() };
                (
                    format!("it changes the gate's own files ({shown}{more}) and its own gate has not certified {tip}"),
                    format!("a PASS from its gate at {tip} (state CERTIFIED) readmits it; a no-verdict or red keeps it out"),
                )
            }
            Cause::NotInForce { dep } => (
                format!("it depends on {dep}, a change to the round machinery that the running release does not yet carry"),
                format!("a release carrying {dep} becoming active readmits it"),
            ),
            Cause::InForceUnread { dep, error } => (
                format!("it depends on {dep}, and whether the running release carries {dep} could not be read: {error}"),
                format!("a later cut that can read whether the release carries {dep} readmits it"),
            ),
            Cause::Uncertified => (
                format!("it is SUBMITTED and not certified at {tip}, which a forge batch needs"),
                format!("a PASS from its gate at {tip}, or a gate verdict there that certifies nothing, readmits it"),
            ),
        };
        format!("batcher {repo}: {id} left out of the cut: {why} — {exit}")
    }
}

fn left_out_dir(run: &Path, repo: &str) -> PathBuf {
    run.join("batcher-left-out").join(repo)
}

/// First sight of each (id, cause, tip) yields its line; `note` leaves it on the bead, and the
/// record is written only once that succeeds, so a failed note is tried again on the next cut.
fn announce_with(
    run: &Path,
    repo: &str,
    left: &[LeftOut],
    note: &mut dyn FnMut(&str, &str) -> Result<(), String>,
) -> Vec<String> {
    let dir = left_out_dir(run, repo);
    let mut lines = Vec::new();
    for l in left {
        let record = format!("{} {}\n", l.tag(), l.tip);
        if fs::read_to_string(dir.join(&l.id)).ok().as_deref() == Some(record.as_str()) {
            continue;
        }
        let line = l.line(repo);
        lines.push(line.clone());
        match note(&l.id, &line) {
            Ok(()) => {
                let _ = fs::create_dir_all(&dir);
                if let Err(e) = fs::write(dir.join(&l.id), record) {
                    eprintln!("batcher {repo}: left-out record of {} unwritable ({e}); it will be noted again", l.id);
                }
            }
            Err(e) => eprintln!("batcher {repo}: {} not noted: {e}", l.id),
        }
    }
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !left.iter().any(|l| l.id == name) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    lines
}

/// Every bead one cut leaves out: the pool's own causes, then each prerequisite refusal, an id
/// once. `tips` holds the tip of each member that reached the refusals.
pub fn cut_left_out(
    mut left: Vec<LeftOut>,
    tips: &BTreeMap<String, String>,
    refusals: &[batcher::core::BlockedRefusal],
) -> Vec<LeftOut> {
    for r in refusals {
        if left.iter().any(|l| l.id == r.id) {
            continue;
        }
        let tip = tips.get(&r.id).cloned().unwrap_or_else(|| "none".into());
        left.push(LeftOut { id: r.id.clone(), tip, cause: Cause::Blocked { dep: r.blocker.clone() } });
    }
    left
}

pub fn announce_left_out(env: &Env, repo: &Repo, left: &[LeftOut]) -> usize {
    let mut failed = 0;
    let lines = announce_with(&env.run, &repo.name, left, &mut |id, text| {
        bead_note(env, id, text).inspect_err(|_| failed += 1)
    });
    for l in lines {
        eprintln!("{l}");
    }
    failed
}

fn moved_tips(pool: &[PoolRow], branches: &BTreeMap<String, String>) -> Vec<LeftOut> {
    pool.iter()
        .filter(|(id, tip, ..)| !tip_current(tip, id, branches))
        .map(|(id, tip, ..)| LeftOut {
            id: id.clone(),
            tip: tip.clone(),
            cause: Cause::TipMoved {
                branch_tip: branches.get(id).cloned().unwrap_or_else(|| "no branch".into()),
            },
        })
        .collect()
}

fn hold_kinds(r: &serde_json::Value) -> Vec<String> {
    let arr = match r.get("holds") {
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(serde_json::Value::String(t)) => serde_json::from_str(t).unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut kinds: Vec<String> = arr.iter().filter_map(|k| k.as_str().map(str::to_string)).collect();
    if kinds.is_empty() {
        kinds.push("unknown".into());
    }
    kinds
}

fn read_pool(env: &Env, repo: &Repo) -> Result<(Vec<PoolRow>, Vec<LeftOut>), String> {
    Ok(pool_rows(&lc_rows(env)?, &repo.name, repo.land))
}

fn pool_rows(rows: &[serde_json::Value], repo: &str, land: Land) -> (Vec<PoolRow>, Vec<LeftOut>) {
    let mut left = Vec::new();
    let mut pool: Vec<PoolRow> = rows
        .iter()
        .filter(|r| r.get("repo").and_then(|v| v.as_str()).is_none_or(|name| name == repo))
        .filter(|r| in_pool_states(r))
        .filter_map(|r| {
            let id = r.get("bead_id")?.as_str()?.to_string();
            let tip = r
                .get("tip")
                .and_then(|t| t.as_str())
                .unwrap_or("none")
                .to_string();
            if !holds_empty(r) {
                left.push(LeftOut { id, tip, cause: Cause::Held(hold_kinds(r)) });
                return None;
            }
            if r.get("ejected_red_tip").and_then(|t| t.as_str()) == Some(tip.as_str()) {
                left.push(LeftOut { id, tip, cause: Cause::EjectedRed });
                return None;
            }
            let epoch = [r.get("since"), r.get("updated_at")]
                .into_iter()
                .flatten()
                .find_map(|v| match v {
                    serde_json::Value::Number(n) => n.as_u64(),
                    serde_json::Value::String(t) => t.parse().ok(),
                    _ => None,
                })
                .unwrap_or(0);
            let express = match r.get("express") {
                Some(serde_json::Value::Bool(b)) => *b,
                Some(serde_json::Value::Number(n)) => n.as_u64() == Some(1),
                Some(serde_json::Value::String(t)) => matches!(t.as_str(), "1" | "true"),
                _ => false,
            };
            let blocked_by = r
                .get("blocked_by")
                .and_then(|b| b.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let provisional = r.get("state").and_then(|s| s.as_str()) == Some("SUBMITTED");
            let reason = r.get("reason").and_then(|s| s.as_str());
            // A forge cut is refused whole for one member the queue would not admit.
            if provisional && land == Land::Forge && !certifies_nothing(BeadState::Submitted, reason) {
                left.push(LeftOut { id, tip, cause: Cause::Uncertified });
                return None;
            }
            Some((id, tip, epoch, express, blocked_by, provisional))
        })
        .collect();
    pool.sort();
    pool.dedup_by(|a, b| a.0 == b.0);
    (pool, left)
}

/// Leaves out each provisional row that changes a file the gate declares its own: such a
/// change enters a cut only once its own gate certified it.
fn gate_own_left_out(
    pool: Vec<PoolRow>,
    own: &gate::def::Def,
    changed: &BTreeMap<String, Vec<String>>,
) -> (Vec<PoolRow>, Vec<LeftOut>) {
    let mut kept = Vec::new();
    let mut left = Vec::new();
    for row in pool {
        let paths: Vec<String> = if row.5 {
            changed.get(&row.0).into_iter().flatten().filter(|p| own.owns(p)).cloned().collect()
        } else {
            Vec::new()
        };
        if paths.is_empty() {
            kept.push(row);
        } else {
            left.push(LeftOut { id: row.0, tip: row.1, cause: Cause::GateUnjudged { paths } });
        }
    }
    (kept, left)
}

/// The provisional rows the gate definition text holds out of the cut. No text (an ungated
/// landing ref) holds out nothing. A definition that does not read, or a diff that cannot be
/// read, holds the row out: a control that cannot check refuses.
fn gate_own_apply(
    pool: Vec<PoolRow>,
    text: Option<&str>,
    changed: &mut dyn FnMut(&PoolRow) -> Result<Vec<String>, String>,
) -> (Vec<PoolRow>, Vec<LeftOut>) {
    let Some(text) = text else {
        return (pool, Vec::new());
    };
    let unjudged = |r: PoolRow, why: String| LeftOut { id: r.0, tip: r.1, cause: Cause::GateUnjudged { paths: vec![why] } };
    let def = match gate::def::parse(text) {
        Ok(d) => d,
        Err(e) => {
            let why = format!("{} on the landing ref does not read: {e}", gate::def::PATH);
            let (provisional, certified): (Vec<_>, Vec<_>) = pool.into_iter().partition(|r| r.5);
            return (certified, provisional.into_iter().map(|r| unjudged(r, why.clone())).collect());
        }
    };
    let mut map = BTreeMap::new();
    let mut left = Vec::new();
    let mut readable = Vec::new();
    for r in pool {
        if !r.5 {
            readable.push(r);
            continue;
        }
        match changed(&r) {
            Ok(paths) => {
                map.insert(r.0.clone(), paths);
                readable.push(r);
            }
            Err(e) => left.push(unjudged(r, format!("its changed paths could not be read: {e}"))),
        }
    }
    let (kept, owned) = gate_own_left_out(readable, &def, &map);
    left.extend(owned);
    (kept, left)
}

/// `gate.steps` at the landing ref: None when the ref has none, Err when git cannot say.
fn gate_def_at(repo: &Repo, rev: &str) -> Result<Option<String>, String> {
    let listed = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["ls-tree", "--name-only", rev, gate::def::PATH]),
        "git ls-tree gate definition",
    )?;
    if listed.trim().is_empty() {
        return Ok(None);
    }
    file_at(repo, rev, gate::def::PATH).map(Some).ok_or_else(|| format!("{} on {rev} could not be read", gate::def::PATH))
}

fn gate_own_check(repo: &Repo, pool: Vec<PoolRow>) -> Result<(Vec<PoolRow>, Vec<LeftOut>), String> {
    if !pool.iter().any(|r| r.5) {
        return Ok((pool, Vec::new()));
    }
    let base_sha = resolve_base_sha(repo)?;
    let text = gate_def_at(repo, &base_sha)?;
    Ok(gate_own_apply(pool, text.as_deref(), &mut |r| changed_paths_checked(repo, &base_sha, &r.1)))
}

/// Beads that carry their commits into whatever stacks on them yet may not be batched
/// themselves: held, or ejected at their current tip.
fn barred_ids(
    env: &Env,
    stacked_on: &[String],
) -> Result<std::collections::BTreeSet<String>, String> {
    let mut rows = lc_rows(env)?;
    let pooled: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter_map(|r| r.get("bead_id")?.as_str())
        .collect();
    let outside: Vec<String> = stacked_on
        .iter()
        .filter(|id| !pooled.contains(id.as_str()))
        .cloned()
        .collect();
    rows.extend(lc_rows_of(env, &outside)?);
    Ok(rows
        .iter()
        .filter(|r| {
            !holds_empty(r)
                || (r.get("ejected_red_tip").and_then(|t| t.as_str()).is_some()
                    && r.get("ejected_red_tip") == r.get("tip"))
        })
        .filter_map(|r| r.get("bead_id")?.as_str().map(str::to_string))
        .collect())
}

/// The pool's own left-out rows whose commits a stacked member would carry into the cut unjudged.
/// Held and ejected rows are [`barred_ids`]'.
fn barred_by_left(left: &[LeftOut]) -> std::collections::BTreeSet<String> {
    left.iter()
        .filter(|l| matches!(l.cause, Cause::Uncertified | Cause::TipMoved { .. }))
        .map(|l| l.id.clone())
        .collect()
}

/// Drops every member whose recorded stack names a barred bead, or a member dropped for that
/// reason, and returns each drop with the prerequisite it stacks on.
fn drop_stacked_on_barred(
    members: Vec<Member>,
    mut barred: std::collections::BTreeSet<String>,
) -> (Vec<Member>, Vec<LeftOut>) {
    let mut kept = members;
    let mut dropped = Vec::new();
    loop {
        let Some(i) = kept
            .iter()
            .position(|m| m.stack.keys().any(|p| barred.contains(p)))
        else {
            return (kept, dropped);
        };
        let m = kept.remove(i);
        let p = m
            .stack
            .keys()
            .find(|p| barred.contains(*p))
            .cloned()
            .unwrap_or_default();
        eprintln!("batcher: skipped {}: stacked on held {p}", m.id);
        dropped.push(LeftOut { id: m.id.clone(), tip: m.tip.clone(), cause: Cause::StackedOn { prereq: p } });
        barred.insert(m.id);
    }
}

/// The description and notes of `id`, where a target and its verdict live — read through
/// lc-serve, never by spawning bd.
pub fn bd_show_text(env: &Env, id: &str) -> Result<String, String> {
    let rows = bd_show(env, &[id.to_string()])?;
    let row = rows
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| format!("plan items: no row for {id}"))?;
    let field = |k: &str| row.get(k).and_then(|v| v.as_str()).unwrap_or("");
    Ok(format!("{}\n{}", field("description"), field("notes")))
}

/// The item rows of `ids` (title, priority, description, notes, external_ref, dependencies as
/// `{depends_on_id, type}`), from `spira-lc plan items --full`.
fn bd_show(env: &Env, ids: &[String]) -> Result<serde_json::Value, String> {
    if ids.is_empty() {
        return Ok(serde_json::Value::Array(vec![]));
    }
    plan_items(&lcq_held(
        env,
        &["plan", "items", "--full", "--ids", &ids.join(",")],
    )?)
}

/// The `items` of a `plan items` answer.
fn plan_items(text: &str) -> Result<serde_json::Value, String> {
    let mut v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("plan items: unparsed output: {e}"))?;
    match v.get_mut("items").map(serde_json::Value::take) {
        Some(items @ serde_json::Value::Array(_)) => Ok(items),
        _ => Err(format!(
            "plan items: no items in {}",
            text.chars().take(200).collect::<String>()
        )),
    }
}

/// `repo`'s own `refs/heads/spira/*` tips, keyed by bead id — the same authority
/// `queue_certified_list` (lib.sh) uses to scope the machine's beads to one
/// repository, and (law-batcher-earns-the-round-by-parity) the batcher's own check that a
/// CERTIFIED record's tip is still the branch's live tip: a branch that moved since
/// certification is not this bead's member until it is re-certified at the new tip, whatever
/// batch.sh did or did not do first.
fn repo_branch_tips(repo: &Repo) -> Result<BTreeMap<String, String>, String> {
    if let Some(tips) = spira_config::git_refs::spira_tips(&repo.path) {
        return Ok(tips);
    }
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads/spira/*",
        ]),
        "git for-each-ref",
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let id = f.next()?.strip_prefix("spira/")?.to_string();
            let sha = f.next()?.to_string();
            Some((id, sha))
        })
        .collect())
}

/// A CERTIFIED record is batchable only while its recorded tip is still the branch's live
/// tip (law-batcher-earns-the-round-by-parity, sp-vafqi). This is the batcher's own second
/// line of defence: whatever wrote or trusted a stale-but-CERTIFIED record upstream, a tip
/// that moved since certification is excluded outright rather than batched at either tip.
fn tip_current(tip: &str, id: &str, branches: &BTreeMap<String, String>) -> bool {
    branches.get(id).map(String::as_str) == Some(tip)
}

/// The round pool for `repo`: every unheld SUBMITTED or CERTIFIED bead whose tip is still the
/// live tip of `refs/heads/spira/<id>` in this repo's own checkout, with title/priority/
/// with express read from the lifecycle row and the rest from one bulk `bd show`.
///
/// Membership is the lifecycle machine's SUBMITTED/CERTIFIED state with no hold, and no gate
/// verdict: the round's full suite is the trial. bd's status and the retired submitted label
/// decide nothing. The sp-1346p shape — a stale
/// record of an incident, an ask, a test bead or work already landed — is a row the machine
/// must not hold CERTIFIED (drop it there), not one this reader second-guesses from bd.
pub fn certified_pool(env: &Env, repo: &Repo) -> Result<Vec<Member>, String> {
    certified_pool_left_out(env, repo).map(|(pool, _)| pool)
}

/// [`certified_pool`] and every bead it left out, for the cut to announce once with the rest.
pub fn certified_pool_left_out(env: &Env, repo: &Repo) -> Result<(Vec<Member>, Vec<LeftOut>), String> {
    let (certified, mut left) = read_pool(env, repo)?;
    let branches = repo_branch_tips(repo)?;
    left.extend(moved_tips(&certified, &branches));
    let mut barred = barred_by_left(&left);
    let certified: Vec<_> = certified
        .into_iter()
        .filter(|(id, tip, ..)| tip_current(tip, id, &branches))
        .collect();
    let (certified, gate_left) = gate_own_check(repo, certified)?;
    barred.extend(gate_left.iter().map(|l| l.id.clone()));
    left.extend(gate_left);
    let ids: Vec<String> = certified.iter().map(|(id, ..)| id.clone()).collect();
    let v = bd_show(env, &ids)?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    let mut by_id: BTreeMap<String, (Option<u8>, String)> = BTreeMap::new();
    let mut texts: BTreeMap<String, (bool, String)> = BTreeMap::new();
    let mut content_blockers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for it in items {
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        content_blockers.insert(id.to_string(), blockers_of(&it));
        let priority = it
            .get("priority")
            .and_then(|p| p.as_u64())
            .map(|p| p.min(9) as u8);
        let title = it
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let own_ref = it
            .get("external_ref")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let text = format!(
            "{title}\n{}",
            it.get("description").and_then(|d| d.as_str()).unwrap_or("")
        );
        texts.insert(
            id.to_string(),
            (
                own_ref.starts_with(&format!("basefail:{}:", repo.name)),
                text,
            ),
        );
        by_id.insert(id.to_string(), (priority, title));
    }
    let fixes = base_fix_ids(env, repo, &texts);
    let content_deps: Vec<String> = content_blockers
        .values()
        .flatten()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let finished = finished_blockers(env, &content_deps);
    let mut out = Vec::new();
    for (id, tip, epoch, express, mut blocked_by, provisional) in certified {
        let Some((priority, title)) = by_id.get(&id) else {
            left.push(LeftOut { id, tip, cause: Cause::NoContentRow });
            continue;
        };
        for dep in content_blockers.get(&id).into_iter().flatten() {
            if !blocked_by.contains(dep) && !finished.contains(dep) {
                blocked_by.push(dep.clone());
            }
        }
        let stack = read_stack(env, &id);
        let base_fix = fixes.contains(&id);
        let priority = if base_fix { Some(0) } else { *priority };
        out.push(Member {
            id,
            tip,
            title: title.clone(),
            priority,
            express,
            base_fix,
            certified_at: epoch,
            stack,
            blocked_by,
            provisional,
        });
    }
    let stacked_on: Vec<String> = out
        .iter()
        .flat_map(|m| m.stack.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    barred.extend(barred_ids(env, &stacked_on)?);
    let (out, waiting) = left_out_not_in_force(env, repo, out, &content_blockers);
    barred.extend(waiting.iter().map(|l| l.id.clone()));
    left.extend(waiting);
    let (kept, stacked) = drop_stacked_on_barred(out, barred);
    left.extend(stacked);
    Ok((kept, left))
}

/// Leaves out each member whose content blocker changes a path the landing ref's gate owns
/// while the running release does not yet carry that change: a blocker in this pool, or one
/// LANDED outside the release. Nothing is checked unless the landing ref's definition has `own`
/// lines and the release is this repo's; what cannot be read leaves the dependent out.
fn left_out_not_in_force(
    env: &Env,
    repo: &Repo,
    members: Vec<Member>,
    content_blockers: &BTreeMap<String, Vec<String>>,
) -> (Vec<Member>, Vec<LeftOut>) {
    let pooled: BTreeMap<String, String> = members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let deps: Vec<String> = members
        .iter()
        .flat_map(|m| content_blockers.get(&m.id).into_iter().flatten())
        .filter(|d| !pooled.contains_key(d.as_str()))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let rows = lc_rows_of(env, &deps);
    let landed: BTreeMap<String, Result<String, String>> = rows
        .iter()
        .flatten()
        .filter(|r| r.get("state").and_then(|s| s.as_str()) == Some("LANDED"))
        .filter(|r| r.get("repo").and_then(|s| s.as_str()).is_none_or(|n| n == repo.name))
        .filter_map(|r| {
            let id = r.get("bead_id")?.as_str()?.to_string();
            let tip = r.get("tip").and_then(|t| t.as_str()).unwrap_or("");
            let sha = if lifecycle::bead::is_full_sha(tip) {
                Ok(tip.to_string())
            } else {
                Err(format!("its LANDED row for {id} carries no full-sha tip (`{tip}`)"))
            };
            Some((id, sha))
        })
        .collect();
    let mut verdict: BTreeMap<String, Result<bool, String>> = BTreeMap::new();
    let mut setup: Option<Result<NotInForceSetup, String>> = None;
    let mut check = |dep: &str| -> Option<Result<bool, String>> {
        let (tip, in_pool) = match (pooled.get(dep), landed.get(dep)) {
            (Some(t), _) => (t.clone(), true),
            (None, Some(Ok(t))) => (t.clone(), false),
            (None, Some(Err(e))) => return Some(Err(e.clone())),
            (None, None) => return rows.as_ref().err().map(|e| Err(format!("its blocker's state could not be read: {e}"))),
        };
        if let Some(v) = verdict.get(dep) {
            return Some(v.clone());
        }
        let setup = setup.get_or_insert_with(|| not_in_force_setup(env, repo));
        let v = match setup {
            Err(e) => Err(e.clone()),
            Ok((None, ..)) => Ok(false),
            Ok((Some(def), base_sha, commit)) => {
                let paths = if in_pool {
                    changed_paths_checked(repo, base_sha, &tip)
                } else {
                    landed_paths(repo, base_sha, commit, &tip).map(Option::unwrap_or_default)
                };
                paths.map(|p| p.iter().any(|p| def.owns(p)))
            }
        };
        verdict.insert(dep.to_string(), v.clone());
        Some(v)
    };
    let mut kept = Vec::new();
    let mut left = Vec::new();
    for m in members {
        let held = content_blockers.get(&m.id).into_iter().flatten().find_map(|d| match check(d) {
            Some(Ok(true)) => Some(Cause::NotInForce { dep: d.clone() }),
            Some(Err(error)) => Some(Cause::InForceUnread { dep: d.clone(), error }),
            None | Some(Ok(false)) => None,
        });
        match held {
            Some(cause) => left.push(LeftOut { id: m.id, tip: m.tip, cause }),
            None => kept.push(m),
        }
    }
    (kept, left)
}

type NotInForceSetup = (Option<gate::def::Def>, String, String);

/// The landing ref's gate definition when it declares `own` lines and the release is this repo's
/// (`None` otherwise), with the landing sha and the release commit.
fn not_in_force_setup(env: &Env, repo: &Repo) -> Result<NotInForceSetup, String> {
    let commit = env.commit.clone()?;
    let release_repo = env.repo.clone()?;
    let base_sha = resolve_base_sha(repo)?;
    let Some(text) = gate_def_at(repo, &base_sha)? else { return Ok((None, base_sha, commit)) };
    let def = gate::def::parse(&text).map_err(|e| format!("{} on the landing ref does not read: {e}", gate::def::PATH))?;
    let ours = release_repo == repo.name && !def.own.is_empty();
    Ok((ours.then_some(def), base_sha, commit))
}

fn is_ancestor(repo: &Repo, a: &str, b: &str) -> Result<bool, String> {
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["merge-base", "--is-ancestor", a, b])
        .output()
        .map_err(|e| format!("git merge-base: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("git merge-base --is-ancestor {a} {b}: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
}

/// The paths a LANDED member's change touched that the release `commit` lacks: `None` when `sha`
/// is off the landing ref or already in `commit`; else its landing merge's diff against its first
/// parent, or `commit...sha` when no merge took it in.
fn landed_paths(repo: &Repo, base_sha: &str, commit: &str, sha: &str) -> Result<Option<Vec<String>>, String> {
    if !is_ancestor(repo, sha, base_sha)? || is_ancestor(repo, sha, commit)? {
        return Ok(None);
    }
    let merges = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["rev-list", "--merges", "--parents", "--reverse", "--ancestry-path", &format!("{sha}..{base_sha}")]),
        "git rev-list landing merges",
    )?;
    let merge = merges.lines().find_map(|l| {
        let mut f = l.split_whitespace();
        let m = f.next()?;
        f.skip(1).any(|p| p == sha).then(|| m.to_string())
    });
    let paths = match merge {
        Some(m) => run(
            Command::new("git").arg("-C").arg(&repo.path).args(["diff", "--name-only", "--no-renames", &format!("{m}^1"), &m]),
            "git diff landing merge",
        )?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect(),
        None => changed_paths_checked(repo, commit, sha)?,
    };
    Ok(Some(paths))
}

/// The ids a content row's `dependencies` block it on. The lifecycle mirror of an edge is
/// best-effort, so the content store is read too: a lagging mirror must not admit a member.
fn blockers_of(row: &serde_json::Value) -> Vec<String> {
    row.get("dependencies")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|d| {
            spira_config::edges::blocks(spira_config::edges::Edge::Dep(
                d.get("type").and_then(|t| t.as_str()).unwrap_or(""),
            ))
        })
        .filter_map(|d| {
            d.get("depends_on_id")
                .and_then(|i| i.as_str())
                .map(str::to_string)
        })
        .collect()
}

/// The certified ids that fix an open base-red bead of `repo`: a bead the basefail bead
/// depends on, one citing it by id, or one carrying the basefail external ref itself. A
/// failed lookup yields none — the lane is an acceleration, never a gate on the round.
fn base_fix_ids(
    env: &Env,
    repo: &Repo,
    texts: &BTreeMap<String, (bool, String)>,
) -> std::collections::BTreeSet<String> {
    // Only a member of the pool can be a fix, so an empty pool has nothing to ask bd about.
    if texts.is_empty() {
        return Default::default();
    }
    let mut fixes: std::collections::BTreeSet<String> = texts
        .iter()
        .filter(|(_, (own, _))| *own)
        .map(|(id, _)| id.clone())
        .collect();
    // The content store lists the base-red beads; whether one is still open is its lifecycle
    // row's — not terminal (sp-mve9i). A base-red bead with no row, or a machine that cannot
    // answer, accelerates nothing: the lane is never a gate.
    let Ok(text) = lcq_held(
        env,
        &[
            "plan",
            "items",
            "--external-contains",
            &format!("basefail:{}:", repo.name),
        ],
    ) else {
        return fixes;
    };
    let Ok(serde_json::Value::Array(rows)) = plan_items(&text) else {
        return fixes;
    };
    let basefail_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .collect();
    let Ok(lc) = lc_rows_of(env, &basefail_ids)
        .and_then(|r| spira_store::lc_state::parse_rows(&serde_json::Value::Array(r).to_string()))
    else {
        return fixes;
    };
    let lc = spira_store::lc_state::index(lc);
    let open: Vec<String> = basefail_ids
        .into_iter()
        .filter(|id| lc.get(id).is_some_and(|row| !row.terminal()))
        .collect();
    for (id, (_, body)) in texts {
        if open.iter().any(|b| body.contains(b.as_str())) {
            fixes.insert(id.clone());
        }
    }
    if let Ok(shown) = bd_show(env, &open) {
        let shown = match shown {
            serde_json::Value::Array(a) => a,
            o => vec![o],
        };
        for b in shown {
            for d in b
                .get("dependencies")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
            {
                let blocks = d.get("type").and_then(|t| t.as_str()) != Some("parent-child");
                if let (true, Some(id)) = (blocks, d.get("depends_on_id").and_then(|i| i.as_str()))
                {
                    fixes.insert(id.to_string());
                }
            }
        }
    }
    fixes.retain(|id| texts.contains_key(id));
    fixes
}

// ---------------------------------------------------------------------------------------
// The open batch: the fields of its record the batcher reads, from `queue batch show`.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct OpenBatch {
    pub pr: String,
    pub members: Vec<(String, String)>,
}

fn parse_kv(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn parse_members(s: &str) -> Vec<(String, String)> {
    s.split_whitespace()
        .map(|t| match t.split_once(':') {
            Some((id, tip)) => (id.to_string(), tip.to_string()),
            None => (t.to_string(), String::new()),
        })
        .collect()
}

/// The repo's open batch from the store's projection: kept current by the process's watch when it
/// runs one, else its batch records read afresh each time (this pass writes batches through the
/// queue between reads; no lifecycle row is reloaded for it); `None` when there is none. Err when the store could not say — a view it cannot vouch for
/// is never read as no batch. Before the cutover (`SPIRA_LC_LIFECYCLE_ON_STORE=0`, the default)
/// the store views refuse, and the record is the queue's: `queue batch show` reads it wherever
/// the queue keeps it.
pub fn read_open_batch(env: &Env, repo: &str) -> Result<Option<OpenBatch>, String> {
    if spira_config::process::cfg_parse::<u8>("SPIRA_LC_LIFECYCLE_ON_STORE")? == 0 {
        let run = queue_verb(env, &["batch", "show", "--repo", repo], "", &[])?;
        if run.code != Some(0) {
            return Err(verb_failed("queue batch show", &run));
        }
        return Ok(open_batch_from(&run.out));
    }
    let views = store_view::Projection::hot(spira_store::lc_call::LcClient::configured()?)?;
    views
        .batches_current()
        .map_err(|e| format!("could not say: {e}"))?;
    open_batch_of(views.batch_view(repo).map(|r| r.value))
}

/// The open batch a view's answer holds; a refused read could not say.
fn open_batch_of(
    read: Result<store_view::BatchView, spira_store::lc_call::LcError>,
) -> Result<Option<OpenBatch>, String> {
    read.map(|v| open_batch_from(&v.shown()))
        .map_err(|e| format!("could not say: {e}"))
}

/// `queue batch show`'s answer: `None` when it printed no record. A record whose PR is not yet
/// recorded (a batch still opening) is an open batch with no PR.
fn open_batch_from(text: &str) -> Option<OpenBatch> {
    let kv = parse_kv(text);
    if kv.is_empty() {
        return None;
    }
    let pr = kv.get("pr").cloned().unwrap_or_default();
    Some(OpenBatch {
        pr,
        members: parse_members(kv.get("members").map(String::as_str).unwrap_or("")),
    })
}

/// Opens the batch PR for `members` at `head` through `queue batch open`, which records the batch
/// before it pushes and creates the PR and arms auto-merge after: the PR's number. The caller holds
/// the repo's queue lock.
pub fn batch_open(
    env: &Env,
    repo: &Repo,
    members: &[(String, String)],
    head: &str,
    body: &str,
) -> Result<(String, Result<(), String>), String> {
    let named = members
        .iter()
        .map(|(id, tip)| format!("{id}@{tip}"))
        .collect::<Vec<_>>()
        .join(",");
    let args = [
        "batch",
        "open",
        "--repo",
        repo.name.as_str(),
        "--members",
        &named,
        "--head",
        head,
        "--owner",
        "batcher",
        "--skip-pregate",
        "--body-file",
        "-",
    ];
    let run = queue_verb(env, &args, body, &[])?;
    print!("{}", run.out);
    eprint!("{}", run.err);
    opened_pr(&run)
}

/// `queue batch open`'s answer: the PR it opened, and Err when the open also failed after it (auto-merge
/// not armed, which the reconciler arms); an Err of its own when it opened none.
fn opened_pr(run: &VerbRun) -> Result<(String, Result<(), String>), String> {
    match (run.code, verb_value(&run.out, "pr")) {
        (Some(0), Some(pr)) => Ok((pr, Ok(()))),
        (_, Some(pr)) => Ok((pr, Err(verb_failed("queue batch open", run)))),
        _ => Err(verb_failed("queue batch open", run)),
    }
}

fn subject_bead(subject: &str) -> Option<&str> {
    fn shaped(t: &str) -> Option<&str> {
        let (stem, rest) = t.split_once('-')?;
        let tail = rest.split('.').next()?;
        let ok = stem.len() >= 2
            && stem.bytes().all(|b| b.is_ascii_lowercase())
            && tail.len() >= 3
            && tail
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && rest
                .split('.')
                .skip(1)
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        ok.then_some(t)
    }
    let lead = subject
        .split_once(':')
        .map(|(a, _)| a.trim())
        .and_then(shaped);
    lead.or_else(|| {
        subject
            .split_once("land ")
            .and_then(|(_, r)| r.split_whitespace().next())
            .map(|t| t.trim_end_matches([':', ',', '.']))
            .and_then(shaped)
    })
}

/// Bead ids named by commit subjects that are not in `members`, sorted and deduplicated.
pub fn foreign_in(subjects: &[String], members: &[String]) -> Vec<String> {
    let mut out: Vec<String> = subjects
        .iter()
        .filter_map(|s| subject_bead(s))
        .filter(|id| !members.iter().any(|m| m == id))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Beads other than `members` that have commits in `base..head`.
pub fn foreign_beads(
    repo: &Repo,
    base: &str,
    head: &str,
    members: &[String],
) -> Result<Vec<String>, String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "log",
            "--format=%s",
            &format!("{base}..{head}"),
        ]),
        "git log base..head",
    )?;
    let subjects: Vec<String> = out.lines().map(str::to_string).collect();
    Ok(foreign_in(&subjects, members))
}

// ---------------------------------------------------------------------------------------
// Section F: when the base ref last moved, tracked across invocations. A batch merge
// commit's own timestamp can predate a member certified moments before it, so "moved" is an
// observation this seam records the moment a fetch shows a new base sha — never read off
// the commit's own clock (core::stale_retry_due takes that timestamp as a given).
// ---------------------------------------------------------------------------------------

pub fn base_moved_at(env: &Env, repo: &str, current_base_sha: &str) -> u64 {
    let p = env.queue_dir.join(repo).join("base-moved");
    let t = now();
    let recorded = fs::read_to_string(&p).ok();
    let (rec_sha, rec_at) = recorded
        .as_deref()
        .and_then(|s| s.split_once(' '))
        .map(|(a, b)| (a.to_string(), b.trim().parse::<u64>().unwrap_or(t)))
        .unwrap_or_default();
    if rec_sha == current_base_sha && !rec_sha.is_empty() {
        return rec_at;
    }
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&p, format!("{current_base_sha} {t}\n"));
    t
}

// ---------------------------------------------------------------------------------------
// Git: worktree assembly, merges, base-conflict classification, pushing.
// ---------------------------------------------------------------------------------------

pub fn resolve_base_sha(repo: &Repo) -> Result<String, String> {
    if let spira_config::git_refs::Ref::Sha(sha) = spira_config::git_refs::read_ref_in(&repo.path, &repo.base) {
        return Ok(sha);
    }
    let out = run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["rev-parse", &repo.base]),
        "git rev-parse base",
    )?;
    Ok(out.trim().to_string())
}

/// Fetch the base's remote so `repo.base` names what the remote holds now. A no-op under
/// `queue.local`, whose base is a local ref. A failed fetch is an error: building on an
/// unrefreshed base is the thing this exists to stop.
pub fn fetch_base(repo: &Repo) -> Result<(), String> {
    if repo.land == Land::Local {
        return Ok(());
    }
    let remote = repo
        .base
        .split_once('/')
        .map(|(r, _)| r)
        .unwrap_or("origin");
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["fetch", "-q", remote]),
        "git fetch base",
    )
    .map(|_| ())
}

/// Re-fetch and return the base the round is about to be opened on, refusing when `head`
/// does not descend from it — the corpus ran on an older base than the one the PR would
/// target, and what lands would be a tree no local run saw.
pub fn confirm_base(repo: &Repo, head: &str) -> Result<String, String> {
    fetch_base(repo)?;
    let base = resolve_base_sha(repo)?;
    if !run_status(Command::new("git").arg("-C").arg(&repo.path).args([
        "merge-base",
        "--is-ancestor",
        &base,
        head,
    ])) {
        return Err(format!(
            "round does not descend from {} ({base}) — the base moved while the round was tested",
            repo.base
        ));
    }
    Ok(base)
}

/// Resets `wt` to `at_sha`, adding the worktree when none stands there. A directory at the path
/// that is no worktree of the repository is cleared first, unless a process works in it; the
/// `Ok` text says what was cleared.
pub fn worktree_reset(repo: &Repo, wt: &Path, at_sha: &str) -> Result<Option<String>, String> {
    let _ = run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["worktree", "prune"]),
        "git worktree prune",
    );
    let cleared = clear_unregistered(repo, wt)?;
    if let Some(c) = &cleared {
        eprintln!("batcher {}: {c}", repo.name);
    }
    if wt.join(".git").exists() {
        run(
            Command::new("git")
                .arg("-C")
                .arg(wt)
                .args(["reset", "-q", "--hard", at_sha]),
            "git reset worktree",
        )?;
        let _ = run(
            Command::new("git")
                .arg("-C")
                .arg(wt)
                .args(["clean", "-qfd"]),
            "git clean worktree",
        );
    } else {
        if let Some(parent) = wt.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        run(
            Command::new("git")
                .arg("-C")
                .arg(&repo.path)
                .args(["worktree", "add", "-q", "--detach"])
                .arg(wt)
                .arg(at_sha),
            "git worktree add",
        )?;
    }
    Ok(cleared)
}

fn clear_unregistered(repo: &Repo, wt: &Path) -> Result<Option<String>, String> {
    if fs::symlink_metadata(wt).is_err() {
        return Ok(None);
    }
    let listed = run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["worktree", "list", "--porcelain"]),
        "git worktree list",
    )?;
    let here = fs::canonicalize(wt).unwrap_or_else(|_| wt.to_path_buf());
    let registered = listed
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| fs::canonicalize(p).unwrap_or_else(|_| p.into()) == here);
    if registered {
        return Ok(None);
    }
    let holders = runtime::holders::holders(Path::new(system_paths::PROC), &here, &[std::process::id()]);
    if !holders.is_empty() {
        return Err(format!(
            "{} is not a worktree of {} and is in use by {}; not clearing it",
            wt.display(),
            repo.name,
            holders.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    let removed = match fs::symlink_metadata(wt) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(wt),
        _ => fs::remove_file(wt),
    };
    removed.map_err(|e| format!("clearing {}: {e}", wt.display()))?;
    Ok(Some(format!(
        "cleared {}: not a worktree of {}, no process using it",
        wt.display(),
        repo.name
    )))
}

/// Is `tip` already an ancestor of the worktree's current HEAD? True for a member reset to
/// an old base, or with no commits of its own — `git merge` on such a tip succeeds as a
/// no-op ("Already up to date") even with `--no-ff`, which is indistinguishable from a real
/// merge by exit status alone, so this must be checked before `merge_member` ever shells out
/// to `git merge`.
fn tip_already_in_round(wt: &Path, tip: &str) -> bool {
    run_status(Command::new("git").arg("-C").arg(wt).args([
        "merge-base",
        "--is-ancestor",
        tip,
        "HEAD",
    ]))
}

/// Merge one member's tip into the worktree with the queue's own commit message. Aborts and
/// reports Conflict on any failure — a dirty merge state must never be left for the next
/// member's attempt. A tip already an ancestor of HEAD is reported Empty without attempting
/// a merge at all, since git would otherwise report success for a no-op.
pub fn merge_member(env: &Env, wt: &Path, id: &str, tip: &str) -> MergeResult {
    if tip_already_in_round(wt, tip) {
        return MergeResult::Empty;
    }
    let subject = land_subject(env, id);
    let ok = run_status(
        Command::new("git")
            .arg("-C")
            .arg(wt)
            .args([
                "-c",
                &format!("user.name={}", env.git_name),
                "-c",
                &format!("user.email={}", env.git_email),
            ])
            .args(["merge", "--no-edit", "--no-ff", "-m", &subject])
            .arg(tip),
    );
    if ok {
        MergeResult::Ok
    } else {
        let _ = run(
            Command::new("git")
                .arg("-C")
                .arg(wt)
                .args(["merge", "--abort"]),
            "git merge --abort",
        );
        MergeResult::Conflict
    }
}

/// The earliest of `winners` (id, tip) that touched one of `files` since `base_sha`: the
/// sibling a base-clean member lost its merge to, with the files they clashed on.
pub fn sibling_conflict(
    repo: &Repo,
    base_sha: &str,
    files: &[String],
    winners: &[(String, String)],
) -> Option<(String, Vec<String>)> {
    for (id, tip) in winners {
        let o = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["diff", "--name-only", &format!("{base_sha}...{tip}")])
            .output()
            .ok()?;
        let touched = String::from_utf8_lossy(&o.stdout).to_string();
        let shared: Vec<String> = files
            .iter()
            .filter(|f| touched.lines().any(|l| l == f.as_str()))
            .cloned()
            .collect();
        if !shared.is_empty() {
            return Some((id.clone(), shared));
        }
    }
    None
}

/// The paths `tip` conflicts on with `base_sha` alone (a real rebase need), or None when it
/// merges cleanly or conflicts only with the batch this round is accumulating.
/// `merge-tree --write-tree` touches no worktree. A conflict is exit 1 with a result tree id
/// on the first line; an unresolvable rev also exits 1 but prints none, and no other failure
/// is evidence of a conflict, so those answer None.
pub fn base_conflict(repo: &Repo, base_sha: &str, tip: &str) -> Option<Vec<String>> {
    conflict_files(&repo.path, base_sha, tip)
}

/// The paths `tip` conflicts on with `side`, per `merge-tree --write-tree`; None when clean.
pub fn conflict_files(dir: &Path, side: &str, tip: &str) -> Option<Vec<String>> {
    let o = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            &format!("attr.tree={side}"),
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            side,
            tip,
        ])
        .output()
        .ok()?;
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let mut lines = out.lines();
    let first = lines.next().unwrap_or("");
    if o.status.code() != Some(1)
        || first.len() < 40
        || !first.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(
        lines
            .take_while(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn conflict_streak_file(env: &Env, id: &str) -> PathBuf {
    env.run.join("conflict-setaside").join(id)
}

/// Records one more round in which `id` at `tip` was set aside for a base conflict and
/// returns how many consecutive rounds that now is. A changed tip starts the count over.
pub fn conflict_streak_bump(env: &Env, id: &str, tip: &str) -> u32 {
    let path = conflict_streak_file(env, id);
    let prev = fs::read_to_string(&path).ok().and_then(|t| {
        let (t, n) = t.trim().split_once(' ')?;
        Some((t.to_string(), n.parse::<u32>().ok()?))
    });
    let n = batcher::core::conflict_streak(prev.as_ref().map(|(t, n)| (t.as_str(), *n)), tip);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&path, format!("{tip} {n}\n"));
    n
}

pub fn conflict_streak_clear(env: &Env, id: &str) {
    let _ = fs::remove_file(conflict_streak_file(env, id));
}

/// Ejects a certified member set aside for a base conflict in consecutive rounds: reopened
/// for rebase, so it can only rejoin by recertifying.
pub fn withdraw_for_conflict(env: &Env, repo: &Repo, id: &str, rounds: u32, files: &[String]) {
    let listed = if files.is_empty() {
        "unknown".to_string()
    } else {
        files.join(", ")
    };
    let reason = format!("set aside for conflict with {} in {rounds} consecutive rounds; conflicting files: {listed}", repo.base);
    bead_reopen(
        env,
        id,
        "rebase-conflict",
        &format!("spira/{id} was {reason} — withdrawn by the batcher for rebase."),
    );
    lc_withdraw(env, id);
    conflict_streak_clear(env, id);
}

/// A SUBMITTED member the round carried to a green corpus has earned CERTIFIED, so `land-local`
/// can walk it Deliver -> Delivered: the round's full suite is its trial, recorded as the gate
/// key. A refusal (tip moved, already certified) is said on stderr and never blocks the round.
pub fn lc_certify_round(env: &Env, members: &[(String, String)], round_key: &str) {
    for (id, tip) in members {
        let Ok(Ok(row)) =
            lcq(env, &["show", id]).map(|o| serde_json::from_str::<serde_json::Value>(&o))
        else {
            continue;
        };
        let bead = row.get("bead").cloned().unwrap_or_default();
        if bead.get("state").and_then(|s| s.as_str()) != Some("SUBMITTED") {
            continue;
        }
        let Some(version) = bead.get("version").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        }) else {
            continue;
        };
        let kind =
            serde_json::json!({"GatePass": {"tip": tip, "gate_key": format!("round:{round_key}")}})
                .to_string();
        if let Err(e) = lcq(
            env,
            &[
                "event",
                "bead",
                id,
                "--expect",
                "SUBMITTED",
                "--version",
                &version.to_string(),
                "--actor",
                "batcher",
                "--kind",
                &kind,
            ],
        ) {
            eprintln!("batcher: LIFECYCLE: {id} not certified by its round on spira-lc: {e}");
        }
    }
}

/// A CERTIFIED member whose tip is already an ancestor of the landing ref is recorded LANDED
/// there, so no later cut sets it aside again. Anything else is left as it stands.
pub fn lc_land_on_base(env: &Env, repo: &Repo, id: &str, tip: &str, base_sha: &str) {
    let fail = |why: String| {
        eprintln!("batcher: LIFECYCLE: {id} not recorded LANDED on spira-lc: {why}")
    };
    if !head_on_base(repo, tip) {
        return;
    }
    let row = match lcq(env, &["show", id]).map(|o| serde_json::from_str::<serde_json::Value>(&o)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return fail(format!("show: unparsed reply: {e}")),
        Err(e) => return fail(format!("show: {e}")),
    };
    let bead = row.get("bead").cloned().unwrap_or_default();
    if bead.get("state").and_then(|s| s.as_str()) != Some("CERTIFIED")
        || bead.get("tip").and_then(|t| t.as_str()) != Some(tip)
    {
        return;
    }
    let Some(version) = bead.get("version").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    }) else {
        return fail("show: no version".into());
    };
    let kind = serde_json::json!({"ContentOnBase": {"proof": format!("ancestry:{base_sha}")}})
        .to_string();
    let version = version.to_string();
    let args = ["event", "bead", id, "--expect", "CERTIFIED", "--version", &version, "--actor", "batcher", "--kind", &kind];
    if let Err(e) = lcq(env, &args) {
        fail(e);
    }
}

/// Takes a member the batcher withdraws out of the pool on the lifecycle machine (sp-mve9i).
/// The pool is the machine's CERTIFIED rows alone — bd's reopen no longer keeps a withdrawn
/// member out of the next round — so the withdrawal is the machine's too, by queue eject's
/// own route: `Deliver`, then `Returned(batch-ejected)`, to REWORK, where the builder's next
/// submit starts a fresh trial. A member not CERTIFIED there (resubmitted since, already
/// returned) is left alone; a refusal is said on stderr and never blocks the round.
pub fn lc_withdraw(env: &Env, id: &str) {
    let fail = |why: String| {
        eprintln!("batcher: LIFECYCLE: {id} not returned to REWORK on spira-lc: {why}")
    };
    let row = match lcq(env, &["show", id]).map(|o| serde_json::from_str::<serde_json::Value>(&o)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return fail(format!("show: unparsed reply: {e}")),
        Err(e) => return fail(format!("show: {e}")),
    };
    let bead = row.get("bead").cloned().unwrap_or_default();
    let mut state = bead
        .get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let Some(mut version) = bead.get("version").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    }) else {
        return fail("show: no version".into());
    };
    if state == "SUBMITTED" {
        let tip = bead.get("tip").and_then(|t| t.as_str()).unwrap_or("");
        let kind =
            serde_json::json!({"GateRed": {"tip": tip, "reason": "suites-failed"}}).to_string();
        if let Err(e) = lcq(
            env,
            &[
                "event",
                "bead",
                id,
                "--expect",
                "SUBMITTED",
                "--version",
                &version.to_string(),
                "--actor",
                "batcher",
                "--kind",
                &kind,
            ],
        ) {
            fail(format!("GateRed: {e}"));
        }
        return;
    }
    if state == "CERTIFIED" {
        let v = version.to_string();
        if let Err(e) = lcq(
            env,
            &[
                "event",
                "bead",
                id,
                "--expect",
                "CERTIFIED",
                "--version",
                &v,
                "--actor",
                "batcher",
                "--kind",
                "\"Deliver\"",
            ],
        ) {
            return fail(format!("Deliver: {e}"));
        }
        state = "IN_DELIVERY".into();
        version += 1;
    }
    if state != "IN_DELIVERY" {
        return;
    }
    let v = version.to_string();
    if let Err(e) = lcq(
        env,
        &[
            "event",
            "bead",
            id,
            "--expect",
            "IN_DELIVERY",
            "--version",
            &v,
            "--actor",
            "batcher",
            "--kind",
            "{\"Returned\":{\"reason\":\"batch-ejected\"}}",
        ],
    ) {
        fail(format!("Returned: {e}"));
    }
}

/// `id`'s state on the machine.
pub fn lc_state(env: &Env, id: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(&lcq(env, &["show", id])?)
        .map_err(|e| format!("show: unparsed reply: {e}"))?;
    v.get("bead")
        .and_then(|b| b.get("state"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| "show: no state".to_string())
}

/// Sends `id` back to REWORK with `GateRed(reason)` at `tip`, but only while it is still the
/// SUBMITTED or CERTIFIED bead at that tip the screen judged.
pub fn lc_gate_red(env: &Env, id: &str, tip: &str, reason: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(&lcq(env, &["show", id])?)
        .map_err(|e| format!("show: unparsed reply: {e}"))?;
    let bead = v.get("bead").cloned().unwrap_or_default();
    let state = bead.get("state").and_then(|s| s.as_str()).unwrap_or("");
    let version = bead
        .get("version")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .ok_or("show: no version")?;
    if !matches!(state, "SUBMITTED" | "CERTIFIED")
        || bead.get("tip").and_then(|t| t.as_str()) != Some(tip)
    {
        return Err(format!("{id} moved since screening (state {state})"));
    }
    let kind = serde_json::json!({"GateRed": {"tip": tip, "reason": reason}}).to_string();
    lcq(
        env,
        &[
            "event",
            "bead",
            id,
            "--expect",
            state,
            "--version",
            &version.to_string(),
            "--actor",
            "sift",
            "--kind",
            &kind,
        ],
    )
    .map(|_| ())
}

/// Records the screen's pass for `tip`; the machine refuses it unless `id` is still SUBMITTED or
/// CERTIFIED at that tip.
fn sifted_accepts(state: &str) -> bool {
    matches!(state, "SUBMITTED" | "CERTIFIED")
}

pub fn lc_sifted(env: &Env, id: &str, tip: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(&lcq(env, &["show", id])?)
        .map_err(|e| format!("show: unparsed reply: {e}"))?;
    let bead = v.get("bead").cloned().unwrap_or_default();
    let state = bead.get("state").and_then(|s| s.as_str()).unwrap_or("");
    if !sifted_accepts(state) {
        println!("batcher: sift: {id} is {state}, not screened at {tip}; no pass recorded");
        return Ok(());
    }
    let version = bead
        .get("version")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .ok_or("show: no version")?;
    let kind = serde_json::json!({"Sifted": {"tip": tip}}).to_string();
    lcq(
        env,
        &[
            "event",
            "bead",
            id,
            "--expect",
            state,
            "--version",
            &version.to_string(),
            "--actor",
            "sift",
            "--kind",
            &kind,
        ],
    )
    .map(|_| ())
}

/// Bead id -> the tip the screen last passed, for every SUBMITTED and CERTIFIED bead that has one.
pub fn sifted_tips(env: &Env) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    {
        for r in lc_rows(env)?.into_iter().filter(in_pool_states) {
            let (Some(id), Some(tip)) = (
                r.get("bead_id").and_then(|x| x.as_str()),
                r.get("sifted_tip").and_then(|x| x.as_str()),
            ) else {
                continue;
            };
            out.insert(id.to_string(), tip.to_string());
        }
    }
    Ok(out)
}

pub fn lc_supersede(env: &Env, id: &str, keeper: &str) -> Result<(), String> {
    let reason = format!("Sift: identical patch (patch-id) to {keeper}.");
    let bin = env
        .lc_bin
        .as_ref()
        .ok_or_else(|| "no spira-lc program".to_string())?;
    // FORK-EDGE: unserved verb close (the spira-lc CLI answers it; the socket does not)
    spira_store::lifecycle_row::close_with(
        &bin.to_string_lossy(),
        id,
        &reason,
        "sift",
        Some(keeper),
    )
}

/// The evidence a send-back stands on, written on the bead before the GateRed.
pub fn bead_note(env: &Env, id: &str, text: &str) -> Result<(), String> {
    // batch-job: bead.sh amend is a bead-store write that takes as long as the store does
    // FORK-EDGE: bead.sh amend is the operator tool (it also mails the dwarf at work on the bead); no library entry does both
    let mut cmd = Command::new("bash");
    cmd.arg(env.home.join("bead.sh"))
        .args(["amend", id, "--note", text])
        .env("SPIRA_HOME", &env.home);
    run(&mut cmd, "bead.sh amend").map(|_| ())
}

/// Suites present at `member_tip` but gone from `base_sha` — reported only for a member set
/// aside on conflict, since that is the list its builder needs to know before rebasing.
pub fn deleted_suites(repo: &Repo, member_tip: &str, base_sha: &str) -> Vec<String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "diff",
            "--diff-filter=D",
            "--name-only",
            member_tip,
            base_sha,
            "--",
            "spira/test-*.sh",
        ]),
        "git diff deleted suites",
    )
    .unwrap_or_default();
    out.lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.rsplit('/').next().unwrap_or(l).to_string())
        .collect()
}

const CONF_REGISTRY: &str = "spira/conf.d/";
const FIXTURE_CONFIG: &str = "spira/testlib/complete.toml";

fn fixture_keys(text: &str) -> std::collections::BTreeSet<String> {
    let doc = match text.parse::<toml::Table>() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("batcher: {FIXTURE_CONFIG} does not parse, its keys are not compared: {}", e.message());
            return Default::default();
        }
    };
    doc.get("spira").and_then(|s| s.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default()
}

/// The config keys `tip` adds and retires since it forked from `base_sha`: the registry files
/// it creates or deletes, and the keys its test fixture config gains or loses.
pub fn config_key_changes(repo: &Repo, base_sha: &str, tip: &str) -> (Vec<String>, Vec<String>) {
    let git = |args: &[&str]| run(Command::new("git").arg("-C").arg(&repo.path).args(args), "git config keys");
    let say = |r: Result<String, String>| r.unwrap_or_else(|e| {
        eprintln!("batcher: config key changes of {tip} unreadable: {e}");
        String::new()
    });
    let (mut added, mut retired) = (Vec::new(), Vec::new());
    let range = format!("{base_sha}...{tip}");
    let status = say(git(&["diff", "--name-status", "--no-renames", &range, "--", CONF_REGISTRY]));
    for l in status.lines() {
        match l.split_once('\t') {
            Some(("A", f)) => added.push(f.trim_start_matches(CONF_REGISTRY).to_string()),
            Some(("D", f)) => retired.push(f.trim_start_matches(CONF_REGISTRY).to_string()),
            _ => {}
        }
    }
    let fork = git(&["merge-base", base_sha, tip]).map(|m| m.trim().to_string()).unwrap_or_else(|_| base_sha.to_string());
    let show = |rev: &str| fixture_keys(&say(git(&["show", &format!("{rev}:{FIXTURE_CONFIG}")])));
    let (before, after) = (show(&fork), show(tip));
    added.extend(after.difference(&before).cloned());
    retired.extend(before.difference(&after).cloned());
    (added, retired)
}

pub const CONFIG_DELTA: &str = "spira/config-delta.toml";
const CONFIG_DELTA_DIR: &str = "spira/config-delta.d";

/// `path` at `rev`; `None` when `rev` has no such file.
pub fn file_at(repo: &Repo, rev: &str, path: &str) -> Option<String> {
    run(Command::new("git").arg("-C").arg(&repo.path).args(["show", &format!("{rev}:{path}")]), "git show file").ok()
}

/// The config delta `rev` declares, base file and fragments merged as every other reader does;
/// `None` when it has none, `Err` when the files conflict or do not parse.
pub fn config_delta_at(repo: &Repo, rev: &str) -> Result<Option<String>, String> {
    let mut parts: Vec<(String, String)> = file_at(repo, rev, CONFIG_DELTA).map(|t| (CONFIG_DELTA.to_string(), t)).into_iter().collect();
    let listed = run(Command::new("git").arg("-C").arg(&repo.path).args(["ls-tree", "--name-only", rev, &format!("{CONFIG_DELTA_DIR}/")]), "git ls-tree config delta").unwrap_or_default();
    let mut names: Vec<&str> = listed.lines().filter(|l| l.ends_with(".toml")).collect();
    names.sort();
    for n in names {
        if let Some(t) = file_at(repo, rev, n) {
            parts.push((n.to_string(), t));
        }
    }
    spira_config::delta::merge_delta_texts(&parts)
}

/// The last commit reachable from `at` that touched any of `paths`, as `<sha> <subject>`.
pub fn last_touching(repo: &Repo, at: &str, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let out = run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["log", "-1", "--format=%h %s", at, "--"])
            .args(paths),
        "git log last touching",
    )
    .ok()?;
    Some(out.trim().to_string()).filter(|l| !l.is_empty())
}

fn diff_names(repo: &Repo, base_sha: &str, tip: &str, no_renames: bool) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&repo.path).args(["diff", "--name-only"]);
    if no_renames {
        cmd.arg("--no-renames");
    }
    let out = run(cmd.arg(format!("{base_sha}...{tip}")), "git diff changed paths")?;
    Ok(out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// Like `changed_paths`, but a rename lists its old path too and a git failure is an error.
pub fn changed_paths_checked(repo: &Repo, base_sha: &str, tip: &str) -> Result<Vec<String>, String> {
    diff_names(repo, base_sha, tip, true)
}

/// The paths `tip` changed since it forked from `base_sha` (`git diff base...tip`) — what
/// attribution orders suspects by and decides a rerun's build by.
pub fn changed_paths(repo: &Repo, base_sha: &str, tip: &str) -> Vec<String> {
    diff_names(repo, base_sha, tip, false).unwrap_or_default()
}

fn patch_id(repo: &Repo, base_sha: &str, tip: &str) -> Option<String> {
    let diff = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["diff", &format!("{base_sha}...{tip}")])
        .output()
        .ok()?;
    if !diff.status.success() {
        return None;
    }
    let mut child = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["patch-id", "--stable"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    std::io::Write::write_all(child.stdin.as_mut()?, &diff.stdout).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string()
    })
}

/// The members whose `refs/heads/spira/<id>` now carries a different patch than the tip the
/// round merged and tested. A rebase alone keeps the patch-id; anything unreadable counts as
/// moved, so the check fails closed.
pub fn moved_members(repo: &Repo, base_sha: &str, members: &[Member]) -> Vec<String> {
    members
        .iter()
        .filter(|m| {
            let live = patch_id(repo, base_sha, &format!("refs/heads/spira/{}", m.id));
            live.is_none() || live != patch_id(repo, base_sha, &m.tip)
        })
        .map(|m| m.id.clone())
        .collect()
}

pub fn set_branch(repo: &Repo, branch: &str, sha: &str) -> Result<(), String> {
    run(
        spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(["branch", "-f", branch, sha]),
        "git branch -f",
    )
    .map(|_| ())
}

pub fn local_branches(repo: &Repo, prefix: &str) -> Vec<String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/heads/{prefix}"),
        ]),
        "git for-each-ref",
    )
    .unwrap_or_default();
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Deletes every local branch under `prefix`; returns the names deleted and, for each that
/// could not be, git's stderr.
pub fn reap_branches(repo: &Repo, prefix: &str) -> (Vec<String>, Vec<(String, String)>) {
    let (mut deleted, mut failed) = (Vec::new(), Vec::new());
    for b in local_branches(repo, prefix) {
        let out = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["branch", "-D", &b])
            .env("SPIRA_REF_SANCTIONED", "1")
            .output();
        match out {
            Ok(o) if o.status.success() => deleted.push(b),
            Ok(o) => failed.push((b, String::from_utf8_lossy(&o.stderr).trim().to_string())),
            Err(e) => failed.push((b, e.to_string())),
        }
    }
    (deleted, failed)
}

/// Removes the repository's attribution branches from earlier rounds, printing each delete
/// that failed with git's reason.
pub fn reap_attribution_branches(who: &str, repo: &Repo) {
    let prefix = format!("spira/batcher-attr/{}-*", repo.name);
    for (b, why) in reap_branches(repo, &prefix).1 {
        eprintln!("{who} {}: could not delete {b}: {why}", repo.name);
    }
}

/// `%Y%m%dT%H%M%SZ`, the stamp every other batch path names its branches with.
pub fn utc_stamp(t: u64) -> String {
    let (days, rem) = ((t / 86400) as i64, t % 86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn head_of(wt: &Path) -> Result<String, String> {
    Ok(run(
        Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["rev-parse", "HEAD"]),
        "git rev-parse HEAD",
    )?
    .trim()
    .to_string())
}

/// How the base moved under a round worktree: `(old, new, landing)` when the base is no longer
/// an ancestor of the worktree's head. `old` is the last commit the two share and `landing` the
/// newest commit since it. `None` while the round still sits on the base.
pub fn base_move(repo: &Repo, wt: &Path) -> Option<(String, String, String)> {
    let git = |args: &[&str]| {
        run(spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(args), "git").ok().map(|s| s.trim().to_string())
    };
    let new = resolve_base_sha(repo).ok()?;
    let head = head_of(wt).ok()?;
    let sits_on_it = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["merge-base", "--is-ancestor", &new, &head])
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    if sits_on_it {
        return None;
    }
    let old = git(&["merge-base", &new, &head])?;
    let landing = git(&["log", "-1", "--format=%h %s", &format!("{old}..{new}")]).unwrap_or_default();
    Some((old, new, landing))
}

/// The member ids `terminal_ready` (core) can treat as actually landed at `head`: every merge
/// commit in `base_sha..head` carries one parent — `merge_member`'s own `--no-ff` always makes
/// the merged tip the second parent, never rewritten by a conflict resolution — so a member
/// whose own certified tip is not among those parents never really merged, or was swapped for
/// a different patch after the corpus run that validated this round.
pub fn named_ids(repo: &Repo, base_sha: &str, head: &str, members: &[Member]) -> Vec<String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "log",
            "--merges",
            "--format=%P",
            &format!("{base_sha}..{head}"),
        ]),
        "git log merges",
    )
    .unwrap_or_default();
    let parents: std::collections::BTreeSet<&str> = out.split_whitespace().collect();
    members
        .iter()
        .filter(|m| parents.contains(m.tip.as_str()))
        .map(|m| m.id.clone())
        .collect()
}

/// Whether the round worktree `wt` holds the release binaries of `head`'s own tree — the
/// same rule `queue land-local --worktree` applies (queue::ops::land::round_bins): the
/// worktree's HEAD tree is `head`'s tree, and its target/release holds an executable.
/// round-vm installs the VM's release build there after checking the VM built this tree.
pub fn bins_present(repo: &Repo, wt: &Path, head: &str) -> bool {
    let Ok(want) = run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["rev-parse", &format!("{head}^{{tree}}")]),
        "git rev-parse tree",
    ) else {
        return false;
    };
    let Ok(have) = run(
        Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["rev-parse", "HEAD^{tree}"]),
        "git rev-parse worktree tree",
    ) else {
        return false;
    };
    if want.trim() != have.trim() {
        return false;
    }
    let dir = wt.join("target").join("release");
    let Ok(rd) = fs::read_dir(&dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let path = e.path();
        path.is_file()
            && fs::metadata(&path)
                .map(|md| md.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------------------
// testenv's per-suite result protocol: the corpus, and where its results land.
// ---------------------------------------------------------------------------------------

/// The round's own suite corpus: every `test-*.sh` committed under `spira/` at `branch`'s
/// tip — read from the git tree, never `repo.path`'s working directory. `repo.path` is the
/// production checkout, which stays on its own branch throughout a round (the round's commits
/// live in a separate worktree, merged into `branch` by `set_branch`); a suite the round
/// added is invisible in that checkout, one it deleted still sits there, and an untracked
/// stray in that checkout is neither — reading the branch's own tree gets all three right at
/// once, matching exactly what testenv-batch.sh validates `--suites` against.
pub fn all_suites(repo: &Repo, branch: &str) -> Result<Vec<String>, String> {
    suites_in(&repo.path, branch)
}

pub fn suites_in(path: &Path, branch: &str) -> Result<Vec<String>, String> {
    let out = run(
        Command::new("git").arg("-C").arg(path).args([
            "ls-tree",
            "-r",
            "--name-only",
            branch,
            "--",
            "spira/",
        ]),
        "git ls-tree suites",
    )?;
    let mut suites: Vec<String> = out
        .lines()
        .filter_map(|l| l.rsplit('/').next())
        .filter(|name| name.starts_with("test-") && name.ends_with(".sh"))
        .map(str::to_string)
        .collect();
    suites.sort();
    suites.dedup();
    Ok(suites)
}

/// The runner writes `<results>/<batch-key>/<suite>.<ext>` (testenv DESIGN.md §9 F1); older
/// callers expected `<results>/<suite>.<ext>`. Look at the top level first, then in the
/// newest batch-key subdirectory that holds the file. `None` when neither exists — the caller
/// treats that as unreached, never green.
pub fn locate(results_dir: &Path, suite: &str, ext: &str) -> Option<PathBuf> {
    let name = format!("{suite}.{ext}");
    let top = results_dir.join(&name);
    if top.is_file() {
        return Some(top);
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(results_dir).ok()?.flatten() {
        let cand = entry.path().join(&name);
        if !cand.is_file() {
            continue;
        }
        let mtime = fs::metadata(&cand)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, cand));
        }
    }
    best.map(|(_, p)| p)
}

/// A suite's final status from its `<suite>.result` (testenv DESIGN.md §3.1): `Some(true)`
/// for a status that does not block (ok, skip, disabled, skip-req, quarantined-red),
/// `Some(false)` for red, timeout, unreached, deferred or fault, and `None` while there is
/// no readable result yet — the file is written after `.out`, so its presence means final.
/// `fault` (sp-3azqi: podman's own exec lost the exit status) is listed with the blocking
/// statuses, not folded into the `_` catch-all meant for "not written yet" — a fault's
/// `.result` line is just as final and complete as a red's, and every caller here already
/// cross-checks its own job's exit code (testenv's contract makes a faulted job's rc 2,
/// never 0 or 1) before trusting `Some(false)` as an ordinary red.
pub fn result_status(results_dir: &Path, suite: &str) -> Option<bool> {
    let path = locate(results_dir, suite, "result")?;
    let text = fs::read_to_string(path).ok()?;
    match text.split_whitespace().next()? {
        "ok" | "skip" | "disabled" | "skip-req" | "quarantined-red" => Some(true),
        "red" | "timeout" | "unreached" | "deferred" | "fault" => Some(false),
        _ => None,
    }
}

pub const EJECT_COLLATERAL_CAUSE: &str = "queue-eject-collateral";

/// Eject one member from the round before it ever reaches CI: reopen its bead and withdraw its
/// certification on the lifecycle machine ([`lc_withdraw`]) with a note naming
/// every suite it turned red, with a distinct cause so census can tell a local ejection from a CI one. `queue-eject-local` is a distinct reopen cause from the retired verdict.sh's `queue-eject`,
/// so census.sh can tell the two apart.
///
/// The suites also go to bead_reopen's fourth argument, which writes the `<id>.ejected`
/// sidecar, as the retired verdict.sh's own ejection did (sp-p3srm): the gate's re-entry check reads it
/// first.
pub fn eject_member(
    env: &Env,
    repo_name: &str,
    id: &str,
    suites: &[String],
    first_fails: &[(String, String)],
    owner: bool,
) {
    let suites_csv = suites.join(",");
    let note = format!(
        "Ejected by the merge queue's local attribution (pre-PR): spira/{id} {} red on: {}.{}",
        if owner {
            "turned"
        } else {
            "is stacked on a member that turned"
        },
        suites.join(", "),
        first_fails
            .iter()
            .map(|(s, l)| format!(" First FAIL, {s}: {l}"))
            .collect::<String>()
    );
    let cause = if owner {
        "queue-eject-local"
    } else {
        EJECT_COLLATERAL_CAUSE
    };
    let _ = lib_call(
        env,
        "bead_reopen",
        [id, cause, note.as_str(), suites_csv.as_str()],
    );
    lc_withdraw(env, id);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env.run.join("landing.log"))
    {
        use std::io::Write;
        let _ = writeln!(
            f,
            "QUEUE LOCAL-EJECT {} repo={repo_name} id={id} suites={suites_csv}",
            now()
        );
    }
}

/// escape-classify.sh over one settled-owner red: `rerun-verdict` when the suite was rerun on
/// the full tree (a green rerun records a flake and says FLIP for the flip policy), otherwise
/// — or on a real red — `classify` then `record`. Returns one line for the batcher's log;
/// every step is best-effort and never blocks the round.
#[allow(clippy::too_many_arguments)]
pub fn record_escape(
    env: &Env,
    repo: &Repo,
    wt: &Path,
    base: &str,
    member: &Member,
    suite: &str,
    batch_id: &str,
    paths: &str,
    evidence: &str,
    rerun_rc: Option<i32>,
) -> String {
    let (classify, child_env) = match spira_config::release_env::tool("escape-classify.sh")
        .and_then(|t| Ok((t, spira_config::release_env::child_path_env_for_process()?)))
    {
        Ok(v) => v,
        Err(e) => return format!("escape not recorded: {e}"),
    };
    let common = |c: &mut Command| {
        c.env("SPIRA_RUN", &env.run).envs(child_env.iter().cloned());
    };
    let record_args = |c: &mut Command| {
        c.args([
            "--member",
            &member.id,
            "--suite",
            suite,
            "--batch-id",
            batch_id,
            "--repo",
            &repo.name,
            "--paths",
            paths,
            "--evidence",
            evidence,
        ]);
    };
    if let Some(rc) = rerun_rc {
        // FORK-EDGE: escape-classify.sh is a 195-line classifier with its own suite; porting it is its own task, not a transport change
        let mut c = Command::new(&classify);
        c.arg("rerun-verdict").args(["--rerun-rc", &rc.to_string()]);
        record_args(&mut c);
        common(&mut c);
        match run(&mut c, "escape-classify.sh rerun-verdict") {
            Ok(out) if out.contains("FLIP") => {
                return format!(
                    "{suite} flipped on rerun with {} present — FLIP: flake recorded",
                    member.id
                )
            }
            Ok(_) => {}
            Err(e) => return format!("escape not recorded: {e}"),
        }
    }
    // FORK-EDGE: escape-classify.sh is a 195-line classifier with its own suite; porting it is its own task, not a transport change
    let mut c = Command::new(&classify);
    c.arg("classify")
        .args(["--repo".as_ref(), repo.path.as_os_str()])
        .args(["--base", base, "--tip", &member.tip]);
    c.arg("--suite-file").arg(wt.join("spira").join(suite));
    c.arg("--gate-log")
        .arg(env.run.join("gate.log"))
        .args(["--branch", &format!("spira/{}", member.id)]);
    common(&mut c);
    let class = match run(&mut c, "escape-classify.sh classify") {
        Ok(out) => out.trim().to_string(),
        Err(e) => return format!("escape not recorded: {e}"),
    };
    // FORK-EDGE: escape-classify.sh is a 195-line classifier with its own suite; porting it is its own task, not a transport change
    let mut c = Command::new(&classify);
    c.arg("record").args(["--class", &class]);
    record_args(&mut c);
    common(&mut c);
    match run(&mut c, "escape-classify.sh record") {
        Ok(_) => format!("{} escaped {suite}: {class}", member.id),
        Err(e) => format!("escape not recorded: {e}"),
    }
}

/// The workspace packages and the workspace packages each depends on (any kind, as unit tests
/// build dev-dependencies too), from `cargo metadata --no-deps` output.
pub fn package_graph(metadata_json: &str) -> Option<batcher::unitattr::PackageGraph> {
    let v: serde_json::Value = serde_json::from_str(metadata_json).ok()?;
    let root = v["workspace_root"].as_str()?.trim_end_matches('/');
    let mut g = batcher::unitattr::PackageGraph::default();
    let packages = v["packages"].as_array()?;
    for p in packages {
        let dir = Path::new(p["manifest_path"].as_str()?).parent()?.to_str()?;
        g.dirs.insert(p["name"].as_str()?.to_string(), dir.strip_prefix(root)?.trim_start_matches('/').to_string());
    }
    for p in packages {
        let deps = p["dependencies"].as_array()?.iter().filter_map(|d| d["name"].as_str()).filter(|n| g.dirs.contains_key(*n)).map(str::to_string).collect();
        g.deps.insert(p["name"].as_str()?.to_string(), deps);
        for t in p["targets"].as_array().into_iter().flatten() {
            let is_test = t["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "test"));
            if let (true, Some(name)) = (is_test, t["name"].as_str()) {
                g.tests.entry(name.to_string()).or_default().push(p["name"].as_str()?.to_string());
            }
        }
    }
    Some(g)
}

#[cfg(test)]
mod local_red_ref_tests {
    use super::local_red_ref;

    #[test_id(tdzjkl3)]
    #[test]
    fn key_is_bounded_and_distinguishes_detail() {
        let long = format!(
            "landing-ref-red red tier: lint; command: {}",
            "x".repeat(2000)
        );
        let s = vec!["local-tier".to_string()];
        let a = local_red_ref("dumpstar", &s, &long);
        assert!(a.len() <= 160, "{a}");
        assert!(a.starts_with("basefail-local:dumpstar:local-tier:landing-ref-red:"));
        assert_ne!(a, local_red_ref("dumpstar", &s, &format!("{long}y")));
        assert_eq!(a, local_red_ref("dumpstar", &s, &long));
    }
}

/// The saved output of each red suite as (suite, text): from `results`, else from the corpus
/// `pass` an attribution follows. A suite with no saved output contributes nothing.
pub fn red_suite_steps(results: &Path, pass: Option<&Path>, suites: &[String]) -> Vec<(String, String)> {
    suites
        .iter()
        .filter_map(|s| {
            let path = locate(results, s, "out").or_else(|| locate(pass?, s, "out"))?;
            Some((s.clone(), fs::read_to_string(path).ok()?))
        })
        .collect()
}

/// The failing tests, findings and compiler errors of the suites a member turned red, recorded on
/// the bead for the builder's next brief. Best-effort: a refusal is said and never blocks the eject.
pub fn record_ejection(env: &Env, id: &str, tip: &str, files: &[String], (results_dir, pass): (&Path, Option<&Path>), suites: &[String], fails: &[(String, String)]) {
    let steps = red_suite_steps(results_dir, pass, suites);
    let why: Vec<String> = fails.iter().map(|(suite, line)| format!("{suite}: {line}")).collect();
    let lc = |args: &[String]| lcq(env, &args.iter().map(String::as_str).collect::<Vec<_>>());
    if let Err(e) = spira_config::ejection::record(&lc, id, tip, files, &why, &steps, "batcher") {
        eprintln!("batcher: {id} failure not recorded on spira-lc: {e}");
    }
}

/// Every regular file of a results directory, and of each directory one level down, named
/// `<prefix>-<file>` or `<prefix>-<dir>-<file>`: send-back entries hold no subdirectory.
fn flat_files(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if path.is_dir() {
            for f in fs::read_dir(&path).into_iter().flatten().flatten().filter(|f| f.path().is_file()) {
                if let Ok(bytes) = fs::read(f.path()) {
                    out.push((format!("{prefix}-{name}-{}", f.file_name().to_string_lossy()), bytes));
                }
            }
        } else if path.is_file() {
            if let Ok(bytes) = fs::read(&path) {
                out.push((format!("{prefix}-{name}"), bytes));
            }
        }
    }
}

/// The files a round's send-back entry holds: the pass's results under `pass-…`, the
/// attribution's own under `attribution-…`.
pub fn send_back_files(evidence: &Path, pass: Option<&Path>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    if let Some(p) = pass {
        flat_files(p, "pass", &mut out);
    }
    flat_files(evidence, "attribution", &mut out);
    out
}

/// The round's send-back entry id: `make` runs on the first call only and its answer is kept in
/// `cache`, so every eject of one process names one entry.
pub fn send_back_entry(cache: &mut Option<String>, make: impl FnOnce() -> Result<String, String>) -> Option<String> {
    if cache.is_none() {
        match make() {
            Ok(id) => *cache = Some(id),
            Err(e) => eprintln!("batcher: send-back logs not kept: {e}"),
        }
    }
    cache.clone()
}

/// Keeps the round's output once and references it from `bead`'s facts.
pub fn reference_round_logs(env: &Env, cache: &mut Option<String>, evidence: &Path, pass: Option<&Path>, bead: &str) {
    let lc = |args: &[String]| lcq(env, &args.iter().map(String::as_str).collect::<Vec<_>>());
    let id = send_back_entry(cache, || {
        let days = spira_config::process::cfg_parse::<u32>("SPIRA_SENDBACK_KEEP_DAYS")?;
        let files = send_back_files(evidence, pass);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        spira_config::sendback::keep(&env.run, "round", &files, days, now)
    });
    if let Some(id) = id {
        if let Err(e) = spira_config::ejection::reference(&lc, &[bead], "round", &id) {
            eprintln!("batcher: {bead} send-back logs not referenced: {e}");
        }
    }
}

/// The first line of a suite's output that reports a failure, trimmed and bounded.
pub fn first_fail_line(out: &str) -> Option<String> {
    let l = out.lines().map(str::trim).find(|l| {
        l.contains("-RED: SIGNAL ")
            || l.contains("-RED: TIMEOUT ")
            || l.contains("-RED: BUILD ")
            || l.starts_with("FAIL")
            || l.contains("FAIL:")
            || l.starts_with("not ok")
    })?;
    Some(l.chars().take(300).collect())
}

/// The first FAIL line of `suite`'s output in the round's results, for an ejection's reason.
pub fn suite_first_fail(results_dir: &Path, pass: Option<&Path>, suite: &str) -> Option<String> {
    let (_, text) = red_suite_steps(results_dir, pass, &[suite.to_string()]).pop()?;
    first_fail_line(&text)
}

/// Copies the round VM's results (every suite's `.out` and `.result`, nested run directories
/// included) into `<run>/rounds/<batch>.results`, where a red round's Concierge and
/// attribution step read them. Returns that directory.
pub fn keep_round_results(run: &Path, batch: &str, from: &Path, prune: Option<&RoundsPrune>) -> PathBuf {
    if let Some(p) = prune {
        p.retention.prune("batcher", &run.join("rounds"), spira_config::vtime::now_epoch(), &spira_config::results_retention::rounds_keep(&p.repo, &p.open));
    }
    let _ = fs::remove_dir_all(round_results_dir(run, batch));
    overlay_round_results(run, batch, from)
}

/// Keeps the pass an attribution follows as the batch's results. A pass already kept there
/// stays as it is: `keep_round_results` empties the destination before it copies.
pub fn keep_attributed_pass(run: &Path, batch: &str, pass: &Path, prune: Option<&RoundsPrune>) {
    let kept = round_results_dir(run, batch);
    let same = pass == kept || matches!((pass.canonicalize(), kept.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if !same {
        keep_round_results(run, batch, pass, prune);
    }
}

/// What a write of a round's kept results may prune from `<run>/rounds`: `repo`'s own
/// `*.results` outside `open`, the open rounds' ids.
pub struct RoundsPrune {
    pub repo: String,
    pub open: Vec<String>,
    pub retention: spira_config::results_retention::Retention,
}

/// The prune for a write of `batch`'s results; None, with the reason on stderr, when the
/// open rounds cannot be listed: a control that cannot check removes nothing.
pub fn rounds_prune(env: &Env, repo: &Repo, batch: &str) -> Option<RoundsPrune> {
    let mut open: Vec<String> = open_rounds(env, repo)
        .map_err(|e| eprintln!("batcher: round results not pruned: the open rounds of {} cannot be listed: {e}", repo.name))
        .ok()?
        .into_iter()
        .map(|r| r.id)
        .collect();
    open.push(batch.to_string());
    Some(RoundsPrune { repo: repo.name.clone(), open, retention: env.results_retention })
}

/// Copies `from` into the round's kept results without removing what is already there; a file
/// of the same name is overwritten, so a re-run suite's newer output replaces the pass's.
pub fn overlay_round_results(run: &Path, batch: &str, from: &Path) -> PathBuf {
    let to = round_results_dir(run, batch);
    fn copy(from: &Path, to: &Path) {
        if fs::create_dir_all(to).is_err() {
            return;
        }
        for e in fs::read_dir(from).into_iter().flatten().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if src.is_dir() {
                copy(&src, &dst);
            } else {
                let _ = fs::copy(&src, &dst);
            }
        }
    }
    copy(from, &to);
    if !to.is_dir() {
        eprintln!(
            "batcher: could not keep the round's results from {} at {}",
            from.display(),
            to.display()
        );
    }
    to
}

/// The end of an attribution: with a pass behind it the pass's kept output stays and the
/// attribution's is laid over it; without one the attribution's output replaces the kept dir.
pub fn keep_attribution_results(run: &Path, batch: &str, from: &Path, after_pass: bool, prune: Option<&RoundsPrune>) -> PathBuf {
    if after_pass {
        overlay_round_results(run, batch, from)
    } else {
        keep_round_results(run, batch, from, prune)
    }
}

/// Puts the deny list an attribution ended with, flip rows included, in the round's kept results,
/// where the round's next certify reads it.
pub fn keep_attribution_deny_list(run: &Path, batch: &str, evidence: &Path) -> Result<(), String> {
    let file = crate::vm::DenySnapshot::FILE;
    let text = fs::read_to_string(evidence.join(file)).map_err(|e| format!("{}: {e}", evidence.join(file).display()))?;
    let kept = round_results_dir(run, batch);
    fs::create_dir_all(&kept).map_err(|e| format!("{}: {e}", kept.display()))?;
    let (tmp, to) = (kept.join(format!("{file}.tmp")), kept.join(file));
    fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, &to)).map_err(|e| format!("{}: {e}", to.display()))
}

/// What a red test phase leaves at the top of a pass's results; round-vm's TEST_PHASE_FILES.
pub const RED_TEST_OUTPUT: [&str; 6] = ["unit.json", "unit-tests.out", "unit-tests-failures.out", "integration.json", "integration-tests.out", "integration-tests-failures.out"];

/// Where a round keeps the red pass's test output, under its own batch-results directory.
pub const RED_PASS_DIR: &str = "red-pass";

/// Copies the red test phases' output of `pass` into `to`, which a later pass of the batch
/// does not overwrite; the dir when it holds any, else None.
pub fn keep_red_test_output(pass: &Path, to: &Path) -> Option<PathBuf> {
    if red_test_results(pass).is_empty() {
        return None;
    }
    fs::create_dir_all(to).ok()?;
    let kept = RED_TEST_OUTPUT.iter().filter(|f| fs::copy(pass.join(f), to.join(f)).is_ok()).count();
    (kept > 0).then(|| to.to_path_buf())
}

/// The bead-body line naming where a red's test output is kept.
pub fn red_output_line(kept: Option<&Path>) -> String {
    match kept {
        Some(d) => format!("Red test output (each failing test's nextest block in *-failures.out): {}", d.display()),
        None => "Red test output: none — the round's local results hold no red test phase".to_string(),
    }
}

pub fn round_results_dir(run: &Path, batch: &str) -> PathBuf {
    run.join("rounds").join(format!("{batch}.results"))
}

/// The failed tests and failure excerpt of each red test phase (`unit.json`, `integration.json`)
/// in the round's results, read from that phase's own output.
fn red_test_results(results: &Path) -> Vec<(&'static str, serde_json::Value)> {
    [("unit.json", "unit-tests"), ("integration.json", "integration-tests")]
        .into_iter()
        .filter_map(|(file, stage)| {
            let v = fs::read_to_string(results.join(file)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())?;
            (v["verdict"] == "red").then_some((stage, v))
        })
        .collect()
}

/// The `unit-tests` / `integration-tests` stages whose result file in `results` says red.
pub fn red_test_phases(results: &Path) -> Vec<String> {
    red_test_results(results).into_iter().map(|(stage, _)| stage.to_string()).collect()
}

fn test_phase_evidence(results: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for (stage, v) in red_test_results(results) {
        for l in v["long_running"].as_array().into_iter().flatten() {
            let end = match l["end"].as_str() {
                Some("unfinished") => "did not finish".to_string(),
                e => format!("then {}", e.unwrap_or("?")),
            };
            lines.push(format!("{} ran over {} s, {end}", l["test"].as_str().unwrap_or("?"), l["over_secs"]));
        }
        let names: Vec<&str> = v["failed"].as_array().into_iter().flatten().filter_map(|n| n.as_str()).collect();
        if !names.is_empty() {
            lines.push(format!("{stage} failed: {}", names.join(", ")));
        }
        if let Some((_, text)) = red_suite_steps(results, None, &[stage.to_string()]).pop() {
            lines.push(format!("{stage}:\n{}", spira_config::sendback::failure_excerpt(&text)));
        }
    }
    lines
}

/// The first `<stage> failed: <names>` line of the newest kept round results under `run`, with
/// the panic line the failure excerpt holds, if any.
pub fn red_test_line(run: &Path) -> Option<String> {
    let newest = fs::read_dir(run.join("rounds")).ok()?.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".results")).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).max()?.1;
    let lines = test_phase_evidence(&newest);
    let failed = lines.iter().find(|l| l.contains(" failed: "))?.clone();
    let panic = lines.iter().flat_map(|l| l.lines()).map(str::trim).find(|l| l.contains("panicked at"));
    Some(panic.map_or(failed.clone(), |p| format!("{failed}; {p}")))
}

/// The failing tests' names and each red suite's first FAIL line, kept under the send-back
/// store, which the next pass's wipe of the round's results does not reach. Returns the kept
/// path, or why nothing was kept.
pub fn keep_eject_evidence(run: &Path, batch: &str, headline: &str, reds: &[String], days: u32) -> Result<(PathBuf, String), String> {
    let results = round_results_dir(run, batch);
    let mut lines: Vec<String> = reds
        .iter()
        .map(|s| {
            let line = suite_first_fail(&results, None, s);
            format!("{s}: {}", line.unwrap_or_else(|| "(no FAIL line in its output)".into()))
        })
        .collect();
    lines.extend(test_phase_evidence(&results));
    let body = lines.join("\n");
    let at = spira_config::sendback::keep_text(run, "red", "red-evidence", &format!("{headline}\n{body}\n"), days)?;
    Ok((at, body))
}

/// Keeps a red round's evidence (see [`keep_eject_evidence`]) and appends its lines to `why`, naming
/// where it is kept (law-a-failure-carries-its-own-explanation).
pub fn keep_red_evidence(run: &Path, batch: &str, why: &str, reds: &[String], days: Result<u32, String>) -> String {
    if reds.is_empty() || batch.is_empty() {
        return why.to_string();
    }
    named_evidence(why, days.and_then(|d| keep_eject_evidence(run, batch, why, reds, d)))
}

fn keep_ejected_red(env: &Env, batch: &str, reason: &str, suites: &[String]) -> String {
    keep_red_evidence(&env.run, batch, reason, suites, spira_config::process::cfg_parse::<u32>("SPIRA_SENDBACK_KEEP_DAYS"))
}

fn named_evidence(why: &str, kept: Result<(PathBuf, String), String>) -> String {
    match kept {
        Ok((p, body)) => format!("{why}\n{body}\nevidence kept: {}", p.display()),
        Err(e) => format!("{why}\nevidence not kept: {e}"),
    }
}

/// The incident key for a local red: a bounded identity plus a short hash of the full reason,
/// because external_ref is a fixed-width column and the reason carries command and error text.
pub fn local_red_ref(repo: &str, suites: &[String], reason: &str) -> String {
    let kind: String = reason
        .split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    let hash = reason.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let suites: String = suites.join(",").chars().take(60).collect();
    format!(
        "basefail-local:{repo}:{suites}:{kind}:{:08x}",
        hash & 0xffff_ffff
    )
}

/// File an Ops incident for a local red the round could not resolve mechanically: a suite red
/// against the base itself, or (defensively) a red attribute.sh could not attribute to
/// anyone. Filed through incident.sh's own dedupe/spool contract, never `bd create` directly,
/// so a flapping base red files one bead and bumps a recurrence rather than one per round.
pub fn file_local_red_incident(
    env: &Env,
    repo: &Repo,
    suites: &[String],
    round_branch: &str,
    evidence: &str,
    reason: &str,
) -> Result<String, String> {
    let title = format!(
        "{}: local round red ({reason}) on {} — nothing can land",
        repo.name,
        suites.join(",")
    );
    let body = batcher::core::base_fail_body(&repo.name, round_branch, suites, evidence);
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("local-red-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        // FORK-EDGE: needs an incident library filing entry that takes the per-call SPIRA_INCIDENT_* fields; intake::file_payload reads them from the process environment only
        Command::new("incident.sh")
            .arg("file")
            .arg(&title)
            .arg(&tmp)
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", "1")
            .env("SPIRA_INCIDENT_ACTOR", "batcher")
            .env("SPIRA_INCIDENT_REPO", &repo.name)
            .env(
                "SPIRA_INCIDENT_REF",
                local_red_ref(&repo.name, suites, reason),
            )
            .env("SPIRA_INCIDENT_CAUSE", local_red_cause(reason)),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    filed_id(&out)
}

/// A round landed green at `head`: the repo's open local-red incidents are over, and
/// incident.sh closes them naming it. A failure is said, never blocks the landing.
pub fn close_superseded_reds(repo: &Repo, head: &str) {
    // FORK-EDGE: needs an incident library entry that closes a repo's local reds; only the incident binary reads the incident store for this
    let mut cmd = Command::new("incident.sh");
    cmd.args(["supersede-red", &repo.name, head]);
    match run(&mut cmd, "incident.sh supersede-red") {
        Ok(said) => println!("batcher {}: {}", repo.name, said.trim()),
        Err(e) => eprintln!("batcher {}: could not close the superseded local reds: {e}", repo.name),
    }
}

/// The distinct beads the test registry's rows name.
pub fn registry_row_beads(env: &Env) -> Result<Vec<String>, String> {
    let bin = env.test_registry_bin.as_ref().ok_or("no test-registry configured")?;
    spira_config::git_refs::held_registry(&format!("row-beads:{}", bin.display()), || registry_row_beads_read(env, bin))
}

fn registry_row_beads_read(env: &Env, bin: &Path) -> Result<Vec<String>, String> {
    // FORK-EDGE: machine leg test-registry (list --json: the registry's rows are read only through its binary)
    let out = spira_config::bounded::bounded(bin)
        .args(["list", "--json"])
        .env("SPIRA_HOME", &env.home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{} list: {e}", bin.display()))?;
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim());
    if !out.status.success() {
        return Err(format!("{} list --json exited {}: {said}", bin.display(), out.status.code().unwrap_or(-1)));
    }
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{} list --json: unparsed reply: {e}", bin.display()))?;
    let rows = body.get("rows").and_then(|r| r.as_array()).ok_or_else(|| format!("{} list --json: no rows in {said}", bin.display()))?;
    let mut beads: Vec<String> = rows.iter().filter_map(|r| r.get("bead").and_then(|b| b.as_str())).filter(|b| !b.is_empty() && *b != "-").map(String::from).collect();
    beads.sort();
    beads.dedup();
    Ok(beads)
}

/// `test-registry clear-landed` for `beads`: the registry rows naming them are over. Returns the cleared test ids.
pub fn clear_landed_rows(env: &Env, beads: &[String]) -> Result<Vec<String>, String> {
    let bin = env.test_registry_bin.as_ref().ok_or("no test-registry configured")?;
    // FORK-EDGE: machine leg test-registry (clear-landed: the row delete and its history row are one transaction only the registry's binary makes)
    let mut cmd = spira_config::bounded::bounded(bin);
    cmd.arg("clear-landed");
    for b in beads {
        cmd.args(["--bead", b]);
    }
    let out = cmd
        .args(["--actor", "batcher"])
        .env("SPIRA_HOME", &env.home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{} clear-landed: {e}", bin.display()))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect());
    }
    Err(format!(
        "{} clear-landed exited {}: {}{}",
        bin.display(),
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn local_red_cause(reason: &str) -> &'static str {
    if reason == spira_config::host_disk::CAUSE {
        spira_config::host_disk::CAUSE
    } else {
        "local-round-red"
    }
}

/// File an Ops incident for a landing whose release was not activated. The members landed, so
/// nothing is filed against them.
pub fn file_deploy_fault_incident(env: &Env, repo: &Repo, head: &str) -> Result<String, String> {
    let title = format!(
        "{}: landed {head} but its release was not activated",
        repo.name
    );
    let body = format!(
        "queue land-local exited {LAND_DEPLOY_FAULT_EXIT} for {head}: the landing is recorded and every member is LANDED, but the release build, verify or activate failed and `current` was not moved. See the queue land-local stderr in the batcher log. Fix the release path; do not reopen the members."
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("deploy-fault-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        // FORK-EDGE: needs an incident library filing entry that takes the per-call SPIRA_INCIDENT_* fields; intake::file_payload reads them from the process environment only
        Command::new("incident.sh")
            .arg("file")
            .arg(&title)
            .arg(&tmp)
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", "1")
            .env("SPIRA_INCIDENT_ACTOR", "batcher")
            .env("SPIRA_INCIDENT_REPO", &repo.name)
            .env(
                "SPIRA_INCIDENT_REF",
                format!("land-deploy-fault:{}:{head}", repo.name),
            )
            .env("SPIRA_INCIDENT_CAUSE", "land-deploy-fault"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    filed_id(&out)
}

/// The bead `incident file` answered with: its stdout is one JSON object, `{"id": <bead>|null}`.
fn filed_id(out: &str) -> Result<String, String> {
    let answer: serde_json::Value = serde_json::from_str(out.trim())
        .map_err(|e| format!("incident.sh file: answer is not JSON ({e})"))?;
    match answer.get("id").and_then(serde_json::Value::as_str) {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => Err("incident.sh file: no id returned".to_string()),
    }
}

fn hold_file(env: &Env, repo: &str) -> PathBuf {
    env.queue_dir.join(repo).join("integration-hold")
}

pub fn write_hold(env: &Env, repo: &str, key: &str, bead: &str) -> Result<(), String> {
    let p = hold_file(env, repo);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, format!("key={key}\nbead={bead}\n"))
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

/// The bead holding this exact round (same members, same tips), while it is not closed — a
/// hold bead, not a work bead, so its bd status is its only state (`nonwork::Kind::Hold`). A
/// bead that cannot be read keeps the hold: a refusal that cannot check must refuse.
pub fn hold_blocking(env: &Env, repo: &str, key: &str) -> Option<String> {
    let kv = parse_kv(&fs::read_to_string(hold_file(env, repo)).ok()?);
    if kv.get("key").map(String::as_str) != Some(key) {
        return None;
    }
    let bead = kv.get("bead").filter(|b| !b.is_empty())?.clone();
    let closed = bd_show(env, std::slice::from_ref(&bead))
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .map(|b| spira_config::nonwork::row_closed(spira_config::nonwork::Kind::Hold, &b))
        .unwrap_or(false);
    (!closed).then_some(bead)
}

/// Folds the round tree's `config-delta.d` fragments into `config-delta.toml` as one commit,
/// so the folded base lands with the round and the directory does not grow. Any failure puts
/// the delta files back as the round merged them: a half-folded tree must not be tested.
/// Returns the sha of the commit it made, or none when there was nothing to fold.
pub fn fold_config_delta(env: &Env, wt: &Path, members: &[Member]) -> Result<Option<String>, String> {
    let git = |args: &[&str]| {
        run(
            spira_config::bounded::bounded("git")
                .arg("-C")
                .arg(wt)
                .args([
                    "-c",
                    &format!("user.name={}", env.git_name),
                    "-c",
                    &format!("user.email={}", env.git_email),
                ])
                .args(args),
            "git",
        )
    };
    let fold = || -> Result<Option<String>, String> {
        let folded = spira_config::delta::fold_fragments(&wt.join("spira"))?;
        if folded.is_empty() {
            return Ok(None);
        }
        git(&["add", "-A", "--", CONFIG_DELTA, CONFIG_DELTA_DIR])?;
        let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
        git(&[
            "commit",
            "-q",
            "-m",
            &format!("round: fold {} config delta fragment(s) into {CONFIG_DELTA}\n\nMembers: {}\n", folded.len(), ids.join(" ")),
        ])?;
        Ok(Some(git(&["rev-parse", "HEAD"])?.trim().to_string()))
    };
    fold().inspect_err(|_| {
        let _ = git(&["reset", "-q", "--hard", "HEAD"]);
    })
}

/// Repairs the integration breaks that are mechanical, on the round tree, as one commit
/// naming every member: a stale test-plan matrix is regenerated and a newly added spira
/// script is made executable. Returns what it fixed (empty means the tree was left untouched)
/// and the sha of the commit it made.
pub fn integration_fix(
    env: &Env,
    wt: &Path,
    base_sha: &str,
    members: &[Member],
) -> Result<(Vec<&'static str>, Option<String>), String> {
    let git = |args: &[&str]| {
        run(
            spira_config::bounded::bounded("git")
                .arg("-C")
                .arg(wt)
                .args(args),
            "git",
        )
    };
    let mut fixes = Vec::new();

    let added = git(&["diff", "--name-only", "--diff-filter=A", base_sha, "HEAD"])?;
    for path in added
        .lines()
        .filter(|p| p.starts_with("spira/") && p.ends_with(".sh"))
    {
        let staged = git(&["ls-files", "-s", "--", path])?;
        if staged.starts_with("100644") {
            git(&["update-index", "--chmod=+x", "--", path])?;
            if !fixes.contains(&"chmod +x new scripts") {
                fixes.push("chmod +x new scripts");
            }
        }
    }

    if wt.join("spira/plan-matrix.sh").is_file() {
        // batch-job: runs a gate, build or forge script that takes as long as its work
        // FORK-EDGE: the round tree's own plan-matrix.sh (that tree's version, not the release's)
        run(
            Command::new("bash")
                .arg(wt.join("spira/plan-matrix.sh"))
                .current_dir(wt),
            "plan-matrix.sh",
        )?;
        if !git(&["status", "--porcelain", "--", "docs/test-plan"])?
            .trim()
            .is_empty()
        {
            git(&["add", "--", "docs/test-plan"])?;
            fixes.push("regenerate the test-plan matrix");
        }
    }

    if fixes.is_empty() {
        return Ok((fixes, None));
    }
    let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let msg = batcher::core::integration_fix_message(&fixes, &ids);
    let mut c = spira_config::bounded::bounded("git");
    c.arg("-C").arg(wt).args([
        "-c",
        &format!("user.name={}", env.git_name),
        "-c",
        &format!("user.email={}", env.git_email),
    ]);
    c.args(["commit", "-q", "-F", "-"])
        .stdin(std::process::Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("git commit: {e}"))?;
    std::io::Write::write_all(
        &mut child.stdin.take().ok_or("git commit: no stdin")?,
        msg.as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("git commit of the integration fix failed".into());
    }
    let sha = git(&["rev-parse", "HEAD"])?.trim().to_string();
    Ok((fixes, Some(sha)))
}

/// The verdict of a round that ejected every member it had: nothing is red any longer, so the
/// rounds loop owes a re-cut, not a pause.
pub const RECUT_VERDICT: &str = "recut";

/// The last local corpus verdict for this repo's round — read by queue's own open-batch so
/// a hand-invoked cut never sends a round CI would only reject (law-a-round-takes-certified-
/// tips). Best-effort like every other queue-dir write here: a failure to record it leaves
/// open-batch with nothing to refuse on, never blocks the round itself.
pub fn write_local_verdict(env: &Env, repo: &str, verdict: &str, detail: &str) {
    let dir = env.queue_dir.join(repo);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let p = dir.join("local-verdict");
    let tmp = p.with_extension("tmp");
    let body = format!("verdict={verdict}\nat={}\ndetail={detail}\n", now());
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

// ---------------------------------------------------------------------------------------
// The forge seam: open a PR, exactly as batch.sh's own pr-create call.
// ---------------------------------------------------------------------------------------

/// The PR body's screenshot section: every PNG a member bead's shots directory holds at the member's
/// tip, linked through Loom when its address is known. Empty when no member has any.
pub fn shots_section(run: &Path, loom_addr: Option<&str>, members: &[(String, String)]) -> String {
    let mut lines = Vec::new();
    for (bead, tip) in members {
        let Ok(rd) = fs::read_dir(run.join("shots").join(bead).join(tip)) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".png"))
            .collect();
        names.sort();
        for n in names {
            let path = format!("{}/{bead}/{tip}/{n}", system_paths::guest::SHOTS);
            lines.push(match loom_addr {
                Some(a) => format!("- {bead} {n}: http://{a}{path}"),
                None => format!("- {bead} {n}: {path}"),
            });
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("\nScreenshots:\n{}\n", lines.join("\n"))
}

/// The batch PR body's screenshot section: `testenv shots --batch` captures the pages the batch's
/// frontend changes name, before and after, and the section links them. A failed capture is logged
/// and becomes a `capture failed: <reason>` line; nothing here can fail the batch.
pub fn batch_shots_section(
    testenv_bin: &Path,
    run_dir: &Path,
    loom_addr: Option<&str>,
    repo: &str,
    base: &str,
    head: &str,
) -> String {
    // FORK-EDGE: machine leg testenv (shots)
    let mut cmd = Command::new("timeout");
    cmd.arg("1500")
        .arg(testenv_bin)
        .args(["shots", "--batch", repo, base, head]);
    if let Err(e) = run(&mut cmd, "testenv shots --batch") {
        eprintln!("batcher {repo}: screenshot capture failed: {e}");
        let reason = e
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output")
            .trim()
            .to_string();
        return format!("\ncapture failed: {reason}\n");
    }
    let pairs = [
        ("batch-before".to_string(), base.to_string()),
        ("batch-after".to_string(), head.to_string()),
    ];
    shots_section(run_dir, loom_addr, &pairs)
}

/// One forge verb: the default forge (`forge`) through its library in this process, `gh` its only
/// child; a configured SPIRA_FORGE path is a fixture's stand-in, run with bash as batch.sh did.
fn forge_run(repo: &Repo, verb: &str, args: &[&str]) -> Result<forge::client::Reply, String> {
    match forge::client::route_of(&repo.forge) {
        forge::client::Route::InProcess => Ok(forge::client::call(verb, &repo.path, args, None)),
        forge::client::Route::Program(script) => {
            // FORK-EDGE: fixture stand-in — a declared SPIRA_FORGE script (suites only; production holds `forge`)
            let o = spira_config::bounded::bounded("bash")
                .arg(&script)
                .arg(verb)
                .arg(&repo.path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| format!("forge {verb}: {e}"))?;
            Ok(forge::client::Reply {
                code: o.status.code().unwrap_or(-1),
                out: String::from_utf8_lossy(&o.stdout).into_owned(),
                err: String::from_utf8_lossy(&o.stderr).into_owned(),
            })
        }
    }
}

/// The reconciler: arms every open batch PR found without auto-merge.
pub fn forge_arm_queue(repo: &Repo) -> Result<(), String> {
    armed(&repo.name, forge_run(repo, "pr-arm-queue", &["merge"])?)
}

fn armed(repo: &str, r: forge::client::Reply) -> Result<(), String> {
    for l in r.out.lines() {
        println!("batcher {repo}: forge {l}");
    }
    if r.ok() {
        Ok(())
    } else {
        Err(format!("forge pr-arm-queue exited {}", r.code))
    }
}

// ---------------------------------------------------------------------------------------
// queue.local's own terminal step (sp-828tp): fast-forward the local landing ref, package and
// activate the round's own --with-bins corpus, mark every member LANDED and close its bead —
// all of it the `queue round` verbs' own contract, never re-derived here.
// ---------------------------------------------------------------------------------------

/// One `queue round <verb>` run. The batcher holds the repo's queue lock for the whole cut, so
/// every verb is told the caller holds it.
pub struct VerbRun {
    pub code: Option<i32>,
    pub out: String,
    pub err: String,
}

/// One `queue <args…>` run, `stdin` on its stdin and `vars` in its environment, told the caller
/// holds the repo's queue lock.
fn queue_verb(
    env: &Env,
    args: &[&str],
    stdin: &str,
    vars: &[(&str, &str)],
) -> Result<VerbRun, String> {
    use std::io::Write;
    // batch-job: this runs whatever its caller names, as long as that takes
    // FORK-EDGE: machine leg queue (batch open/round verbs — the batcher's writes through the queue)
    let mut cmd = Command::new(&env.queue_bin);
    cmd.args(args).envs(vars.iter().copied());
    cmd.env("SPIRA_QUEUE_LOCK_HELD", "1");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let what = format!(
        "queue {}",
        args.iter().take(2).copied().collect::<Vec<_>>().join(" ")
    );
    let mut child = cmd.spawn().map_err(|e| format!("{what}: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        match si.write_all(stdin.as_bytes()) {
            Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                return Err(format!("{what}: stdin: {e}"));
            }
            _ => {}
        }
    }
    let o = child
        .wait_with_output()
        .map_err(|e| format!("{what}: {e}"))?;
    Ok(VerbRun {
        code: o.status.code(),
        out: String::from_utf8_lossy(&o.stdout).to_string(),
        err: String::from_utf8_lossy(&o.stderr).to_string(),
    })
}

fn round_verb(env: &Env, args: &[&str], stdin: &str) -> Result<VerbRun, String> {
    round_verb_with(env, args, stdin, &[])
}

/// The round verbs that name who acts on the round (`queue round <verb> --by <actor>`).
const ACTED_VERBS: [&str; 6] = ["open", "certify", "eject", "land", "abandon", "promote"];

/// The whole argv after `queue` for a round verb: the batcher acts, so it says so.
pub fn round_argv(args: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = std::iter::once("round")
        .chain(args.iter().copied())
        .map(String::from)
        .collect();
    if args.first().is_some_and(|v| ACTED_VERBS.contains(v)) && !args.contains(&"--by") {
        all.extend(["--by".to_string(), BATCHER_ACTOR.to_string()]);
    }
    all
}

fn round_verb_with(
    env: &Env,
    args: &[&str],
    stdin: &str,
    vars: &[(&str, &str)],
) -> Result<VerbRun, String> {
    let all = round_argv(args);
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let run = queue_verb(env, &all, stdin, vars)?;
    if !run.out.is_empty() {
        print!("{}", run.out);
    }
    if !run.err.is_empty() {
        eprint!("{}", run.err);
    }
    Ok(run)
}

fn verb_value(out: &str, key: &str) -> Option<String> {
    out.lines().find_map(|l| {
        l.strip_prefix(key)
            .and_then(|v| v.strip_prefix('='))
            .map(|v| v.trim().to_string())
    })
}

fn verb_failed(what: &str, run: &VerbRun) -> String {
    format!(
        "{what} exited {}: {}",
        run.code.map_or("by signal".to_string(), |c| c.to_string()),
        refusal_line(&run.err)
    )
}

/// `queue round open` for `members` in `wt`: the batch id, once the verb admitted and merged
/// exactly the members asked for. Any other outcome is an Err and, when a round was opened, it
/// is abandoned first — the batcher never works a round it did not name.
pub fn round_open(env: &Env, repo: &Repo, wt: &Path, members: &[Member]) -> Result<String, String> {
    match round_open_or_refused(env, repo, wt, members)? {
        OpenOutcome::Opened(batch) => Ok(batch),
        OpenOutcome::Refused(r, line) => Err(format!(
            "{line} (refused: {})",
            r.iter()
                .map(|r| format!("{} {}", r.id, r.kind))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// One member a refused `queue round open` named as not admitted, from its typed stdout record
/// (`refused=<id> kind=<kind> why=<line>`, `queue::ops::round::refused_record`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub id: String,
    pub kind: String,
    pub why: String,
}

/// Every typed refusal record in a `queue round open` stdout.
pub fn parse_refused(out: &str) -> Vec<Refused> {
    out.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("refused=")?;
            let (id, rest) = rest.split_once(" kind=")?;
            let (kind, why) = rest.split_once(" why=").unwrap_or((rest, ""));
            Some(Refused {
                id: id.trim().to_string(),
                kind: kind.trim().to_string(),
                why: why.trim().to_string(),
            })
        })
        .collect()
}

/// What `queue round open` came to: the batch it opened with exactly the named members, or the
/// members it refused (each named, with its kind) and the refusal's line, nothing opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    Opened(String),
    Refused(Vec<Refused>, String),
}

/// [`round_open`], with a refusal that names its members returned as [`OpenOutcome::Refused`]
/// so the caller re-cuts without them instead of failing the cut.
pub fn round_open_or_refused(
    env: &Env,
    repo: &Repo,
    wt: &Path,
    members: &[Member],
) -> Result<OpenOutcome, String> {
    let text = members.iter().fold(String::new(), |mut acc, m| {
        acc.push_str(&format!("{}:{}\n", m.id, m.tip));
        acc
    });
    let wt_arg = wt.display().to_string();
    let run = round_verb(
        env,
        &[
            "open",
            &repo.name,
            "--members-file",
            "-",
            "--worktree",
            &wt_arg,
        ],
        &text,
    )?;
    if run.code != Some(0) {
        let refused = parse_refused(&run.out);
        if refused.is_empty() {
            return Err(verb_failed("queue round open", &run));
        }
        return Ok(OpenOutcome::Refused(
            refused,
            verb_failed("queue round open", &run),
        ));
    }
    let batch = verb_value(&run.out, "batch").ok_or("queue round open named no batch")?;
    let opened: Vec<String> = verb_value(&run.out, "members")
        .unwrap_or_default()
        .split_whitespace()
        .map(|m| m.split(':').next().unwrap_or("").to_string())
        .collect();
    let wanted: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    if opened != wanted {
        round_abandon(
            env,
            repo,
            &batch,
            "the round opened with members other than the ones the batcher named",
            "members-differ",
        );
        return Err(format!(
            "queue round open admitted [{}], the batcher named [{}] — round {batch} abandoned",
            opened.join(" "),
            wanted.join(" ")
        ));
    }
    Ok(OpenOutcome::Opened(batch))
}

/// The suites to name when the repository's own ci is red: the ones its testenv declares, which
/// is the only vocabulary `queue round eject --suites` accepts. A tree declaring none is judged
/// as a whole, which the verb knows as `testenv`.
pub fn ci_red_suites(wt: &Path) -> Vec<String> {
    match gate::own::known_suites(wt) {
        Ok(known) if !known.is_empty() => known,
        _ => vec!["testenv".to_string()],
    }
}

/// `queue round eject`: `owner` is a member whose own tip reddened `suites`; a collateral
/// member leaves uncharged. The batcher rebuilds the round tree itself, so the verb does not.
pub fn round_eject(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    suites: &[String],
    first_fails: &[(String, String)],
    owner: bool,
) -> Result<(), String> {
    let reason = eject_reason(id, suites, first_fails, owner, None);
    let reason = if owner { keep_ejected_red(env, batch, &reason, suites) } else { reason };
    let csv = if owner {
        suites.join(",")
    } else {
        String::new()
    };
    round_eject_with(env, repo, batch, id, &reason, &csv)
}

/// `round_eject` for the member a mender's return named: `lead` opens the reason and the member owns the red.
pub fn round_eject_returned(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    suites: &[String],
    first_fails: &[(String, String)],
    lead: &str,
) -> Result<(), String> {
    let reason = eject_reason(id, suites, first_fails, true, Some(lead));
    let reason = keep_ejected_red(env, batch, &reason, suites);
    round_eject_with(env, repo, batch, id, &reason, &suites.join(","))
}

pub fn returned_lead(suites: &[String], pass: u32, batch: &str, why: &str) -> String {
    format!("Returned by the mender of {} in pass {pass} of {batch}: {why}", suites.join(", "))
}

/// A mender's return leads with its diagnosis; any other ejection says which red it turned.
pub fn eject_reason(id: &str, suites: &[String], first_fails: &[(String, String)], owner: bool, returned: Option<&str>) -> String {
    let fails: String = first_fails.iter().map(|(s, l)| format!(" First FAIL, {s}: {l}")).collect();
    match returned {
        Some(lead) => format!("{lead}{fails}"),
        None => format!(
            "Ejected by the merge queue's local attribution (pre-PR): spira/{id} {} red on: {}.{fails}",
            if owner { "turned" } else { "is stacked on a member that turned" },
            suites.join(", ")
        ),
    }
}

/// `queue round eject --suites` for the only member of a round the repository's own CI judged red:
/// nothing else in the round can own the red, and `evidence` (the verdict and the output path)
/// is the reason the rework is held to.
pub fn round_eject_sole_red(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    suites: &[String],
    evidence: &str,
) -> Result<(), String> {
    let reason = format!("Ejected by the merge queue's local round: spira/{id} is the only member of a round the repository's own CI judged red, so the red is its own. Evidence: {evidence}");
    round_eject_with(env, repo, batch, id, &reason, &suites.join(","))
}

/// `queue round eject` for a member whose code lacks what `behind` adds: it leaves the round
/// holding the compiler error as its reason, and a blocks edge keeps the next cut from admitting
/// it before they have landed.
pub fn round_sequence(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    behind: &[String],
    line: &str,
) -> Result<(), String> {
    let reason = format!(
        "Held behind {} by the merge queue's local attribution (pre-PR): the round's workspace failed to build because spira/{id} uses what {} adds. It is admitted again once {} landed. Compiler: {line}",
        behind.join(", "),
        behind.join(", "),
        if behind.len() == 1 { "it has" } else { "they have" }
    );
    round_eject_with(env, repo, batch, id, &reason, "workspace-build")?;
    sequence_edges(env, id, behind)
}

/// The blocks edges that record a sequence: `id` depends on each of `behind`.
pub fn sequence_edges(env: &Env, id: &str, behind: &[String]) -> Result<(), String> {
    for dep in behind {
        // batch-job: bead.sh dep add is a bead-store write that takes as long as the store does
        // FORK-EDGE: bead.sh dep add refuses the edges that would deadlock (an epic, an incident); no library entry does
        let mut cmd = Command::new("bash");
        cmd.arg(env.home.join("bead.sh"))
            .args(["dep", "add", id, dep])
            .env("SPIRA_HOME", &env.home);
        run(&mut cmd, "bead.sh dep add")?;
    }
    Ok(())
}

/// Whether `tip` adds `path`, which `base_sha` lacks.
pub fn adds_path(dir: &Path, base_sha: &str, tip: &str, path: &str) -> bool {
    run(
        Command::new("git").arg("-C").arg(dir).args([
            "diff",
            "--name-only",
            "--diff-filter=A",
            &format!("{base_sha}...{tip}"),
            "--",
            path,
        ]),
        "git diff added paths",
    )
    .map_err(|e| eprintln!("batcher: cannot tell whether {tip} adds {path}: {e}"))
    .is_ok_and(|out| out.lines().any(|l| l == path))
}

/// The lines `tip` adds over `base_sha`, as `git diff` prints them.
pub fn added_lines(repo: &Repo, base_sha: &str, tip: &str) -> String {
    run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "diff",
            "-U0",
            &format!("{base_sha}...{tip}"),
        ]),
        "git diff added lines",
    )
    .unwrap_or_default()
    .lines()
    .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
    .map(|l| format!("{l}\n"))
    .collect()
}

/// Members of `ids` whose lifecycle row carries a red gate verdict that bars its tip and whose gate has not passed, as `(id, tip, the row's mark)`.
pub fn gate_red_rows(env: &Env, ids: &[String]) -> Vec<(String, String, String)> {
    ids.iter()
        .filter_map(|id| {
            let row =
                serde_json::from_str::<serde_json::Value>(&lcq(env, &["show", id]).ok()?).ok()?;
            gate_red_of(id, row.get("bead")?)
        })
        .collect()
}

/// Whether the gate's mark (its reason, then any failing lines) names a red that bars the tip: a timeout or a no-rebase ejects nobody.
pub fn mark_bars_tip(mark: &str) -> bool {
    lifecycle::bead::mark_bars_tip(mark.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or(""))
}

/// The ejectable red mark on one lifecycle row: IN_DELIVERY, its own gate not passed (no gate key), and a reason that bars the tip, so a timeout, a no-rebase or a passed gate preempts nothing.
fn gate_red_of(id: &str, bead: &serde_json::Value) -> Option<(String, String, String)> {
    let field = |k: &str| bead.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let mark = field("reason").strip_prefix(lifecycle::bead::GATE_RED_MARK)?;
    let gate_passed = bead.get("gate_key").is_some_and(|k| !k.is_null());
    (field("state") == "IN_DELIVERY" && !gate_passed && lifecycle::bead::mark_bars_tip(mark))
        .then(|| (id.to_string(), field("tip").to_string(), mark.to_string()))
}

/// Members of an open round whose lifecycle row carries a red gate verdict at the very tip the
/// round holds, as `(id, the row's mark)`: the gate already judged this tree red, so the round's
/// corpus need not rediscover it.
pub fn gate_red_marks(env: &Env, members: &[(String, String)]) -> Vec<(String, String)> {
    let ids: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
    gate_red_rows(env, &ids)
        .into_iter()
        .filter(|(id, tip, _)| members.iter().any(|(m, t)| m == id && t == tip))
        .map(|(id, _, mark)| (id, mark))
        .collect()
}

/// `queue round eject` for a member the gate had already marked red while it sat in the round.
pub fn round_eject_gate_red(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    mark: &str,
) -> Result<(), String> {
    let reason = format!(
        "Ejected from the round before its corpus ran: spira/{id}'s own gate went red at the tip the round holds ({mark}). The gate's whole output is kept under the gate-worker's output directory."
    );
    round_eject_with(env, repo, batch, id, &reason, "")
}

/// The arguments after `queue round` for an eject; the reason comes on stdin. No csv names no
/// suite.
pub fn eject_args(batch: &str, repo: &str, id: &str, csv: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "eject",
        batch,
        id,
        repo,
        "--reason-file",
        "-",
        "--by",
        "batcher",
    ]
    .map(String::from)
    .to_vec();
    if !csv.is_empty() {
        args.extend(["--suites".to_string(), csv.to_string()]);
    }
    args
}

fn round_eject_with(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    reason: &str,
    csv: &str,
) -> Result<(), String> {
    let args = eject_args(batch, &repo.name, id, csv);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let attempts = env.land_lock_attempts.max(1);
    let mut last = round_verb(env, &args, reason)?;
    for n in 1..attempts {
        if !lock_busy(last.code) {
            break;
        }
        println!(
            "batcher {}: queue round eject refused ({}) — retry {n}/{attempts}",
            repo.name,
            refusal_line(&last.err)
        );
        std::thread::sleep(env.land_lock_wait);
        last = round_verb(env, &args, reason)?;
    }
    eject_result(&last, id, reason, csv)
}

/// Must equal queue::ops::round::PASS_RUNNING: a pass of the round is running, nothing changed.
pub const QUEUE_PASS_RUNNING_EXIT: i32 = 79;

/// Starts the error of an eject that was held, not refused: what follows is a [`HeldEject`].
pub const EJECT_HELD: &str = "eject-held=";

/// An eject to issue again, verbatim, once no pass of the round is running.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeldEject {
    pub id: String,
    pub reason: String,
    pub csv: String,
    pub said: String,
}

impl HeldEject {
    /// The held eject an error carries, wherever in the text its marker sits.
    pub fn parse(err: &str) -> Option<HeldEject> {
        let at = err.find(EJECT_HELD)?;
        serde_json::from_str(err[at + EJECT_HELD.len()..].lines().next()?).ok()
    }
}

fn eject_result(run: &VerbRun, id: &str, reason: &str, csv: &str) -> Result<(), String> {
    match run.code {
        Some(0) => Ok(()),
        Some(QUEUE_PASS_RUNNING_EXIT) => {
            let held = HeldEject { id: id.into(), reason: reason.into(), csv: csv.into(), said: verb_failed("queue round eject", run) };
            Err(format!("{EJECT_HELD}{}", serde_json::to_string(&held).unwrap_or_default()))
        }
        _ => Err(verb_failed("queue round eject", run)),
    }
}

/// The eject a [`HeldEject`] stands for, issued again.
pub fn round_eject_held(env: &Env, repo: &Repo, batch: &str, held: &HeldEject) -> Result<(), String> {
    round_eject_with(env, repo, batch, &held.id, &held.reason, &held.csv)
}

/// `queue round certify`: with `Some((head, ran))` the batcher's own corpus ran `ran` suites green
/// on `head`, the round worktree's head, and the verb records that tree's GREEN; with `None` the
/// verb runs the repo's own CI entry on a round VM and judges the round itself.
pub fn round_certify(
    env: &Env,
    repo: &Repo,
    batch: &str,
    attest: Option<(&str, usize)>,
    rehead: Option<&str>,
) -> Certified {
    let ran = attest.map(|(_, n)| n.to_string());
    let mut args = vec!["certify", batch, repo.name.as_str()];
    if let (Some((head, _)), Some(ran)) = (attest, ran.as_deref()) {
        args.extend(["--attest", head, "--ran", ran]);
    }
    if let Some(h) = rehead {
        args.extend(["--rehead", h]);
    }
    let run = match round_verb(env, &args, "") {
        Ok(r) => r,
        Err(e) => return Certified::Failed(e.into()),
    };
    if run.code == Some(0) {
        return Certified::Green;
    }
    if let Some(output) = verb_value(&run.out, "red-output") {
        return Certified::Red { output };
    }
    let failed = verb_failed("queue round certify", &run);
    Certified::Failed(certify_failure(run.code, &run.err, failed))
}

fn certify_failure(code: Option<i32>, stderr: &str, failed: String) -> CutError {
    if stderr.contains(crate::pace::STALE_AMI_MARKER) {
        CutError::new(
            CutKind::StaleImage,
            format!("{}: {failed}", crate::pace::STALE_AMI_MARKER),
        )
    } else if matches!(code, Some(CERTIFY_ENV_RC | CERTIFY_LOST_RC)) {
        CutError::new(CutKind::Infra, failed)
    } else {
        failed.into()
    }
}

/// `queue round certify`'s exit code for an environment fault: no verdict on the round.
pub const CERTIFY_ENV_RC: i32 = 4;

/// `queue round certify`'s exit code when the round VM was lost mid-run: no verdict either.
pub const CERTIFY_LOST_RC: i32 = 75;

pub enum Certified {
    Green,
    /// The repository's own CI judged the round red; `output` is the file holding its output.
    Red {
        output: String,
    },
    Failed(CutError),
}

/// The members whose changed files the red output names; a lone member owns any red.
pub fn implicated(output: &str, changed: &[(String, Vec<String>)]) -> Vec<String> {
    if let [(id, _)] = changed {
        return vec![id.clone()];
    }
    changed
        .iter()
        .filter(|(_, paths)| paths.iter().any(|p| output.contains(p.as_str())))
        .map(|(id, _)| id.clone())
        .collect()
}

/// The member ids the round record holds now, as `queue round status` reports them.
pub fn round_members(env: &Env, repo: &Repo, batch: &str) -> Result<Vec<String>, String> {
    let run = round_verb(env, &["status", &repo.name], "")?;
    if run.code != Some(0) {
        return Err(verb_failed("queue round status", &run));
    }
    members_of_batch(&run.out, batch)
        .ok_or_else(|| format!("queue round status names no open round {batch}"))
}

/// The member ids `queue round status` reports for `batch`, whichever of the open rounds it is:
/// the first is unprefixed, the rest are `round<n>_`.
fn members_of_batch(out: &str, batch: &str) -> Option<Vec<String>> {
    open_rounds_of(out)
        .into_iter()
        .find(|r| r.id == batch)
        .map(|r| r.members)
}

/// The id of the repo's open round, if `queue round status` reports one.
pub fn open_round_id(env: &Env, repo: &Repo) -> Result<Option<String>, String> {
    let run = round_verb(env, &["status", &repo.name], "")?;
    if run.code != Some(0) {
        return Err(verb_failed("queue round status", &run));
    }
    if verb_value(&run.out, "round").as_deref() != Some("open") {
        return Ok(None);
    }
    Ok(Some(
        open_rounds_of(&run.out)
            .into_iter()
            .next()
            .map(|r| r.id)
            .unwrap_or_default(),
    ))
}

/// The actor the batcher passes as `--by` to the queue verbs it acts with.
pub const BATCHER_ACTOR: &str = "batcher";

/// One round `queue round status` lists as open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenRound {
    pub id: String,
    pub members: Vec<String>,
    /// Each member's tip, in `members` order.
    pub tips: Vec<String>,
    pub head: String,
    pub worktree: String,
    pub phase: String,
    pub red: bool,
    /// The suites the queue recorded red for the round's pass, from the round record's `red` key.
    pub red_suites: Vec<String>,
    /// The beads `queue round eject` has taken out of the round, from the record's `ejected` key.
    pub ejected: Vec<String>,
    /// Who ejected each bead of `ejected`, in the same order, from the record's `ejected_by` key.
    pub ejected_by: Vec<String>,
    pub age_secs: u64,
}

/// The suite names of a round record's `red` value.
fn red_suites_of(red: &str) -> Vec<String> {
    red.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// The bead ids of a round record's `ejected` value: space-separated, as `queue round eject` writes it.
fn ejected_of(ejected: &str) -> Vec<String> {
    ejected.split_whitespace().map(String::from).collect()
}

/// `members` as `queue round status` writes them, `id:tip` apiece, split into ids and tips.
fn split_members(members: &str) -> (Vec<String>, Vec<String>) {
    members.split_whitespace().map(|m| m.split_once(':').map_or((m, ""), |(i, t)| (i, t))).map(|(i, t)| (i.to_string(), t.to_string())).unzip()
}

/// Every open round of the repo, as `queue round status` lists them: the first is unprefixed,
/// the rest are `round<n>_`.
pub fn open_rounds(env: &Env, repo: &Repo) -> Result<Vec<OpenRound>, String> {
    if let Some(views) = store_view::shared() {
        let now = spira_config::vtime::now_epoch();
        return views
            .round_view(&repo.name)
            .map_err(|e| format!("could not say: {e}"))
            .map(|v| v.value.rounds.iter().map(|r| open_round_of(r, now)).collect());
    }
    let run = round_verb(env, &["status", &repo.name], "")?;
    if run.code != Some(0) {
        return Err(verb_failed("queue round status", &run));
    }
    Ok(open_rounds_of(&run.out))
}

/// A round of the store's view as `queue round status` lists it.
pub fn open_round_of(r: &store_view::Round, now: u64) -> OpenRound {
    let get = |k: &str| r.record.get(k).unwrap_or_default();
    let (members, tips) = split_members(get("members"));
    OpenRound {
        id: r.record.id.clone(),
        members,
        tips,
        head: get("head").to_string(),
        worktree: get("worktree").to_string(),
        phase: r.phase.clone(),
        red: !get("red").is_empty(),
        red_suites: red_suites_of(get("red")),
        ejected: ejected_of(get("ejected")),
        ejected_by: ejected_of(get("ejected_by")),
        age_secs: get("opened").parse().map_or(0, |t: u64| now.saturating_sub(t)),
    }
}

fn open_rounds_of(out: &str) -> Vec<OpenRound> {
    let prefixes = std::iter::once(String::new()).chain((2..).map(|n| format!("round{n}_")));
    prefixes
        .map_while(|p| {
            let id = verb_value(out, &format!("{p}batch_id"))?;
            let get = |k: &str| verb_value(out, &format!("{p}{k}")).unwrap_or_default();
            let (members, tips) = split_members(&get("members"));
            Some(OpenRound {
                id,
                members,
                tips,
                head: get("head"),
                worktree: get("worktree"),
                phase: if p.is_empty() {
                    verb_value(out, "phase").unwrap_or_default()
                } else {
                    get("phase")
                },
                red: !get("red").is_empty(),
                red_suites: red_suites_of(&get("red")),
                ejected: ejected_of(&get("ejected")),
                ejected_by: ejected_of(&get("ejected_by")),
                age_secs: get("wall_secs").parse().unwrap_or(0),
            })
        })
        .collect()
}

/// `Ok` when `batch`'s worktree still stands at the round's head, which is all `queue round certify`
/// needs of it; otherwise why a certify would be refused. One batcher worktree serves every round
/// of a repo, so a later round moves it off an earlier one.
pub fn round_certifiable(env: &Env, repo: &Repo, batch: &str) -> Result<(), String> {
    let (head, worktree) = open_rounds(env, repo)?
        .into_iter()
        .find(|r| r.id == batch)
        .map(|r| (r.head, r.worktree))
        .ok_or_else(|| format!("queue round status names no open round {batch}"))?;
    let at = git_head(Path::new(&worktree))
        .ok_or_else(|| format!("the round worktree {worktree:?} has no readable head"))?;
    if at == head {
        Ok(())
    } else {
        Err(format!(
            "the round worktree {worktree} is at {at}, not at the round's head {head}"
        ))
    }
}

fn git_head(dir: &Path) -> Option<String> {
    let o = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
}

/// Whether `batch` is open and still in phase `opened`: no certify has ever moved it on.
pub fn round_is_opened(env: &Env, repo: &Repo, batch: &str) -> bool {
    open_rounds(env, repo).is_ok_and(|rs| rs.iter().any(|r| r.id == batch && r.phase == "opened"))
}

/// `round-recert --batch <id> --repo <repo>`: starts the round's driver in its own unit and returns.
pub fn redrive_round(repo: &Repo, batch: &str) -> Result<(), String> {
    // FORK-EDGE: machine leg round-recert
    let out = run(
        spira_config::bounded::bounded_for(spira_config::release_env::tool("round-recert")?, "60")
            .args(["--batch", batch, "--repo", &repo.name]),
        "round-recert",
    )?;
    println!("{}", out.trim());
    Ok(())
}

/// One incident for a round that sits open with nothing to drive it, keyed by the round so a
/// pass that finds it again is a recurrence of the same incident.
pub fn file_orphan_round_incident(
    env: &Env,
    repo: &Repo,
    batch: &str,
    why: &str,
) -> Result<String, String> {
    let title = format!("{}: round {batch} sits open with no driver", repo.name);
    let body = format!(
        "Round {batch} of {} has been open with no certify running for over {} s: {why}.\nThe rounds loop never re-cuts an open round. Reset the worktree to the round's head and run `round-recert --batch {batch} --repo {}`, or eject or abandon the round. Rounds stacked on it are refused at land until it ends.",
        repo.name,
        env.orphan_redrive_after,
        repo.name
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("round-orphan-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        // FORK-EDGE: needs an incident library filing entry that takes the per-call SPIRA_INCIDENT_* fields; intake::file_payload reads them from the process environment only
        Command::new("incident.sh")
            .arg("file")
            .arg(&title)
            .arg(&tmp)
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", "1")
            .env("SPIRA_INCIDENT_ACTOR", "batcher")
            .env("SPIRA_INCIDENT_REPO", &repo.name)
            .env(
                "SPIRA_INCIDENT_REF",
                format!("round-orphan:{}:{batch}", repo.name),
            )
            .env("SPIRA_INCIDENT_CAUSE", "round-orphan"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    filed_id(&out)
}

/// The repo's local verdict as `write_local_verdict` recorded it: (verdict, at).
pub fn read_local_verdict(env: &Env, repo: &str) -> Option<(String, u64)> {
    let kv = parse_kv(&fs::read_to_string(env.queue_dir.join(repo).join("local-verdict")).ok()?);
    Some((kv.get("verdict")?.clone(), kv.get("at")?.parse().ok()?))
}

/// `cause` is the one word the round's `.result` line carries as `abandoned:<cause>`.
/// A host pass records its start on the batch before its first long step, as the EC2 place does,
/// so the store sees a round that has begun, its suites phase included. Best effort: a refusal is logged and the pass runs.
pub fn round_pass_start(env: &Env, repo: &Repo, batch: &str) {
    if let Err(e) = try_round_pass_start(env, repo, batch) {
        eprintln!("batcher {}: {e}", repo.name);
    }
}

/// [`round_pass_start`], with the refusal returned: the batcher's own leg does not start its unit on a pass the queue did not record.
pub fn try_round_pass_start(env: &Env, repo: &Repo, batch: &str) -> Result<(), String> {
    let verbs: [(&str, Vec<&str>); 2] = [("pass-start", vec!["pass-start", "--resume", batch, &repo.name]), ("suites-started", vec!["suites-started", batch, &repo.name])];
    for (name, args) in &verbs {
        match round_verb(env, args, "") {
            Ok(r) if r.code == Some(0) => {}
            Ok(r) => return Err(verb_failed(&format!("queue round {name}"), &r)),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// An attribution that ended without an outcome leaves its failure text on the batch as the
/// pass's incomplete verdict; a pass that already has its verdict ignores it.
pub fn round_attribution_failed(env: &Env, repo: &Repo, batch: &str, why: &str) {
    round_pass_incomplete(env, repo, batch, &format!("the attribution leg failed: {why}"));
}

/// The corpus pass ended red without the queue hearing a verdict: recording it takes the round
/// out of `CI_RUNNING`, where no eject is admitted.
pub fn round_corpus_ended_red(env: &Env, repo: &Repo, batch: &str) {
    round_pass_incomplete(env, repo, batch, "the corpus pass ended red; its members are attributed");
}

/// The corpus re-run the attribution opened ended: its pass is incomplete, and certify owns any green.
pub fn round_attribution_rerun_ended(env: &Env, repo: &Repo, batch: &str) {
    round_pass_incomplete(env, repo, batch, "the attribution's corpus re-run ended; its members are probed");
}

fn round_pass_incomplete(env: &Env, repo: &Repo, batch: &str, reason: &str) {
    match round_verb(env, &["pass-verdict", "--verdict", "incomplete", "--reason", reason, batch, &repo.name], "") {
        Ok(r) if r.code == Some(0) => {}
        Ok(r) => eprintln!("batcher {}: {}", repo.name, verb_failed("queue round pass-verdict", &r)),
        Err(e) => eprintln!("batcher {}: {e}", repo.name),
    }
}

/// What the batcher tells `queue round abandon` it considered. It claims ejections only for a
/// known count above none, and says none were made only for a known zero.
pub fn abandon_considered(cause: &str, ejected: Option<usize>) -> String {
    match ejected {
        None => format!("the batcher's {cause} abandon"),
        Some(0) => format!("the batcher's {cause} abandon; no member was ejected"),
        Some(n) => format!("the batcher's {cause} abandon, after ejecting {n} member(s) it could implicate"),
    }
}

/// [`round_abandon_ejecting`] for a caller that does not know how many members were ejected first.
pub fn round_abandon(env: &Env, repo: &Repo, batch: &str, why: &str, cause: &str) -> bool {
    round_abandon_ejecting(env, repo, batch, why, cause, None)
}

/// `ejected` is how many members the caller ejected from the round before abandoning it, when it knows.
pub fn round_abandon_ejecting(env: &Env, repo: &Repo, batch: &str, why: &str, cause: &str, ejected: Option<usize>) -> bool {
    round_abandon_considered(env, repo, batch, why, cause, &abandon_considered(cause, ejected))
}

/// [`round_abandon_ejecting`] with the text `queue round abandon` is told the batcher considered.
pub fn round_abandon_considered(env: &Env, repo: &Repo, batch: &str, why: &str, cause: &str, considered: &str) -> bool {
    if spira_config::round_ended::is_ended(&env.run, batch) {
        println!("batcher {}: round {batch} has already ended — no second abandon", repo.name);
        let files = crate::pace::Files::new(&env.run, &repo.name);
        if files.abandon_refused().is_some_and(|(b, _, _)| b == batch) {
            files.clear_abandon_refused();
        }
        return true;
    }
    let vars = [("QUEUE_ABANDON_CONSIDERED", considered)];
    let files = crate::pace::Files::new(&env.run, &repo.name);
    let refused = match round_verb_with(
        env,
        &[
            "abandon",
            batch,
            &repo.name,
            "--reason-file",
            "-",
            "--cause",
            cause,
        ],
        why,
        &vars,
    ) {
        Ok(r) if r.code == Some(0) => {
            if files.abandon_refused().is_some_and(|(b, _, _)| b == batch) {
                files.clear_abandon_refused();
            }
            return true;
        }
        Ok(r) => verb_failed("queue round abandon", &r),
        Err(e) => e,
    };
    println!(
        "batcher {}: could not abandon round {batch}: {refused}",
        repo.name
    );
    if let Err(e) = files.record_abandon_refused(batch, cause, why) {
        println!("batcher {}: could not record the refused abandon: {e}", repo.name);
    }
    false
}

/// The round whose abandon is still refused, once the abandon is tried again: `Some` while it
/// stays open, so a cut holds rather than stack another round on it. A round that is gone
/// (abandoned by hand, or landed) clears the record. When the queue cannot say, it holds.
pub fn abandon_hold(env: &Env, repo: &Repo) -> Option<String> {
    let files = crate::pace::Files::new(&env.run, &repo.name);
    let (batch, cause, why) = files.abandon_refused()?;
    match open_rounds(env, repo) {
        Ok(rs) if !rs.iter().any(|r| r.id == batch) => {
            files.clear_abandon_refused();
            None
        }
        Ok(_) if round_abandon(env, repo, &batch, &why, &cause) => None,
        _ => Some(batch),
    }
}

/// A `queue round` verb running beside the caller, its output in files so a full pipe cannot
/// stall it. The staged round's two long verbs run this way: `stage` while the open round's
/// suites run, `stage-test` while it lands.
pub struct BgVerb {
    child: std::process::Child,
    what: String,
    out: PathBuf,
    err: PathBuf,
}

fn round_verb_bg(env: &Env, args: &[&str], stdin: &str) -> Result<BgVerb, String> {
    use std::io::Write;
    let what = format!("queue round {}", args.first().copied().unwrap_or(""));
    let stem = env.run.join(format!(
        "round-bg-{}-{}",
        std::process::id(),
        args.first().copied().unwrap_or("verb")
    ));
    let (out, err) = (stem.with_extension("out"), stem.with_extension("err"));
    let file = |p: &Path| fs::File::create(p).map_err(|e| format!("{what}: {}: {e}", p.display()));
    // batch-job: this runs whatever its caller names, as long as that takes
    // FORK-EDGE: machine leg queue (batch open/round verbs — the batcher's writes through the queue)
    let mut cmd = Command::new(&env.queue_bin);
    cmd.arg("round").args(args);
    cmd.env("SPIRA_QUEUE_LOCK_HELD", "1");
    cmd.stdin(Stdio::piped())
        .stdout(file(&out)?)
        .stderr(file(&err)?);
    let mut child = cmd.spawn().map_err(|e| format!("{what}: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(stdin.as_bytes())
            .map_err(|e| format!("{what}: stdin: {e}"))?;
    }
    Ok(BgVerb {
        child,
        what,
        out,
        err,
    })
}

impl BgVerb {
    pub fn wait(mut self) -> Result<VerbRun, String> {
        let status = self
            .child
            .wait()
            .map_err(|e| format!("{}: {e}", self.what))?;
        let read = |p: &Path| {
            let t = fs::read_to_string(p).unwrap_or_default();
            let _ = fs::remove_file(p);
            t
        };
        let run = VerbRun {
            code: status.code(),
            out: read(&self.out),
            err: read(&self.err),
        };
        if !run.out.is_empty() {
            print!("{}", run.out);
        }
        if !run.err.is_empty() {
            eprint!("{}", run.err);
        }
        Ok(run)
    }
}

/// `queue round stage` for `members` behind the open round, in `wt`.
pub fn round_stage_spawn(
    env: &Env,
    repo: &Repo,
    wt: &Path,
    members: &[Member],
) -> Result<BgVerb, String> {
    let text = members.iter().fold(String::new(), |mut acc, m| {
        acc.push_str(&format!("{}:{}\n", m.id, m.tip));
        acc
    });
    let wt_arg = wt.display().to_string();
    round_verb_bg(
        env,
        &[
            "stage",
            &repo.name,
            "--members-file",
            "-",
            "--worktree",
            &wt_arg,
        ],
        &text,
    )
}

/// The staged round's batch id once the verb wrote the row; any other end is `None` and the
/// open round goes on as if nothing had been staged.
pub fn round_stage_done(repo: &Repo, bg: BgVerb) -> Option<String> {
    match bg.wait() {
        Ok(run) if run.code == Some(0) => verb_value(&run.out, "batch"),
        Ok(run) => {
            println!(
                "batcher {}: {} — no round staged",
                repo.name,
                verb_failed("queue round stage", &run)
            );
            None
        }
        Err(e) => {
            println!("batcher {}: {e} — no round staged", repo.name);
            None
        }
    }
}

pub fn round_stage_test_spawn(env: &Env, repo: &Repo) -> Result<BgVerb, String> {
    round_verb_bg(env, &["stage-test", &repo.name], "")
}

/// True when the staged round's own pass ended green.
pub fn round_stage_test_done(repo: &Repo, bg: BgVerb) -> bool {
    match bg.wait() {
        Ok(run) if run.code == Some(0) => true,
        Ok(run) => {
            println!(
                "batcher {}: {} — the staged round gets its own pass",
                repo.name,
                verb_failed("queue round stage-test", &run)
            );
            false
        }
        Err(e) => {
            println!("batcher {}: {e}", repo.name);
            false
        }
    }
}

/// What `queue round promote` cut: the round now OPEN, its head, its members, and whether the
/// staged pass was attested as that round's certification.
#[derive(Debug, PartialEq, Eq)]
pub struct Promotion {
    pub batch: String,
    pub head: String,
    pub members: Vec<(String, String)>,
    pub attested: bool,
}

#[derive(serde::Deserialize)]
struct PromotionAnswer {
    batch: String,
    head: String,
    members: Vec<PromotedMember>,
    attested: bool,
}

#[derive(serde::Deserialize)]
struct PromotedMember {
    id: String,
    tip: String,
}

/// The verb's answer is its last non-empty stdout line, one JSON object; the lines before it
/// are the certification pass's progress.
pub fn parse_promotion(out: &str) -> Option<Promotion> {
    let line = out.lines().rev().find(|l| !l.trim().is_empty())?;
    let a: PromotionAnswer = serde_json::from_str(line.trim()).ok()?;
    Some(Promotion {
        batch: a.batch,
        head: a.head,
        members: a.members.into_iter().map(|m| (m.id, m.tip)).collect(),
        attested: a.attested,
    })
}

/// `queue round promote`. A refusal is not an error: the verb has discarded the stage, or left
/// it for `round discard`, and the next cut proceeds from the pool as ever.
pub fn round_promote(env: &Env, repo: &Repo) -> Option<Promotion> {
    match round_verb(env, &["promote", &repo.name], "") {
        Ok(run) if run.code == Some(0) => parse_promotion(&run.out).or_else(|| {
            println!("batcher {}: queue round promote named no round", repo.name);
            None
        }),
        Ok(run) => {
            println!(
                "batcher {}: {}",
                repo.name,
                verb_failed("queue round promote", &run)
            );
            None
        }
        Err(e) => {
            println!("batcher {}: {e}", repo.name);
            None
        }
    }
}

/// Exit 1 is the verb's own refusal — reported, not an error; exit 3 is a deploy fault: the
/// members landed, only the release was not activated.
#[derive(Debug, PartialEq, Eq)]
pub enum LandOutcome {
    Landed,
    Refused,
    DeployFault,
}

/// Must equal queue::ops::DEPLOY_FAULT.
pub const LAND_DEPLOY_FAULT_EXIT: i32 = 3;

pub fn classify_land_exit(code: Option<i32>) -> LandOutcome {
    match code {
        Some(0) => LandOutcome::Landed,
        Some(LAND_DEPLOY_FAULT_EXIT) => LandOutcome::DeployFault,
        _ => LandOutcome::Refused,
    }
}

/// What one `round_land` call came to: the outcome, and the line that explains a refusal.
#[derive(Debug, PartialEq, Eq)]
pub struct LandRun {
    pub outcome: LandOutcome,
    pub refusal: String,
    /// The verb's exit, which classifies a refusal ([`land_base_moved`]).
    pub code: Option<i32>,
}

/// Must equal queue::ops::LAND_BASE_MOVED: the round's base moved, and the verb rebuilt (or
/// could not rebuild) it on the new one; it is certified again, not landed.
pub const LAND_BASE_MOVED_EXIT: i32 = 8;

/// Whether a `queue round land` refusal was base-moved, read from its exit
/// (law-lifecycle-state-is-not-text).
pub fn land_base_moved(code: Option<i32>) -> bool {
    code == Some(LAND_BASE_MOVED_EXIT)
}

/// Must equal queue::ops::LOCK_BUSY: another queue operation held the repo's lock past the wait.
pub const QUEUE_LOCK_BUSY_EXIT: i32 = 6;

/// Whether a queue verb's refusal was a busy lock, to be retried: read from its exit code
/// (law-lifecycle-state-is-not-text). Every other refusal is final.
pub fn lock_busy(code: Option<i32>) -> bool {
    code == Some(QUEUE_LOCK_BUSY_EXIT)
}

/// The line a refusal is reported by, for the operator: the last non-empty stderr line.
fn refusal_line(err: &str) -> String {
    err.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(no stderr)")
        .trim()
        .to_string()
}

/// A refusal because another queue operation holds the lock is retried (bounded); every other
/// outcome is final.
pub fn round_land(env: &Env, repo: &Repo, batch: &str) -> Result<LandRun, String> {
    let attempts = env.land_lock_attempts.max(1);
    let mut last = LandRun {
        outcome: LandOutcome::Refused,
        refusal: String::new(),
        code: None,
    };
    for n in 1..=attempts {
        let run = round_verb(env, &["land", batch, &repo.name], "")?;
        last = LandRun {
            outcome: classify_land_exit(run.code),
            refusal: refusal_line(&run.err),
            code: run.code,
        };
        let busy = lock_busy(run.code);
        if !busy || n == attempts {
            break;
        }
        println!(
            "batcher {}: queue round land refused ({}) — retry {n}/{attempts}",
            repo.name, last.refusal
        );
        std::thread::sleep(env.land_lock_wait);
    }
    Ok(last)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Unpause {
    Off,
    Absent,
    Cleared,
    Held,
}

/// A deliberate hold (the file starts with `hold:`) belongs to the operator; any other pause is
/// the loop's own and a landing ends it.
pub fn unpause_after_land(env: &Env) -> Result<Unpause, String> {
    let Some(path) = env.land_unpause.as_ref() else {
        return Ok(Unpause::Off);
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(Unpause::Absent);
    };
    if text.starts_with("hold:") {
        return Ok(Unpause::Held);
    }
    fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Unpause::Cleared)
}

/// True only when `head` is an ancestor of the repo's landing ref: the one fact that says the
/// round landed, whatever land-local reported.
pub fn head_on_base(repo: &Repo, head: &str) -> bool {
    run_status(Command::new("git").arg("-C").arg(&repo.path).args([
        "merge-base",
        "--is-ancestor",
        head,
        &repo.base,
    ]))
}

/// The (title, ref) an unlanded round is filed under. A refusal because the round is stacked on one
/// that has not landed has the unlanded head as its one cause, whichever round asks next, so those
/// share a key and incident.sh counts them as recurrences of one incident.
pub fn land_unverified_key(repo: &Repo, head: &str, detail: &str) -> (String, String) {
    if let Some(parent) = stacked_parent(detail) {
        return (
            format!(
                "{}: rounds stacked on unlanded round {parent} are refused at land",
                repo.name
            ),
            format!("land-unverified:{}:stacked:{parent}", repo.name),
        );
    }
    (
        format!(
            "{}: round head {head} did not reach {}",
            repo.name, repo.base
        ),
        format!("land-unverified:{}:{head}", repo.name),
    )
}

fn stacked_parent(detail: &str) -> Option<&str> {
    let rest = detail.split(" is stacked on round ").nth(1)?;
    let (parent, _) = rest.split_once(", which has not landed")?;
    (!parent.is_empty() && !parent.contains(char::is_whitespace)).then_some(parent)
}

/// Alarm for a round that did not land: the incident names the refusal line.
pub fn file_land_unverified_incident(
    env: &Env,
    repo: &Repo,
    head: &str,
    detail: &str,
) -> Result<String, String> {
    let (title, incident_ref) = land_unverified_key(repo, head, detail);
    let body = format!(
        "queue land-local did not land round head {head} on {}: {detail}\nNo member was landed by this round; the batcher reported the round as refused. Find why land-local refused (its stderr is in the batcher log) before the next round.",
        repo.base
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("land-unverified-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        // FORK-EDGE: needs an incident library filing entry that takes the per-call SPIRA_INCIDENT_* fields; intake::file_payload reads them from the process environment only
        Command::new("incident.sh")
            .arg("file")
            .arg(&title)
            .arg(&tmp)
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", "1")
            .env("SPIRA_INCIDENT_ACTOR", "batcher")
            .env("SPIRA_INCIDENT_REPO", &repo.name)
            .env("SPIRA_INCIDENT_REF", incident_ref)
            .env("SPIRA_INCIDENT_CAUSE", "land-unverified"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    filed_id(&out)
}

// ---------------------------------------------------------------------------------------
// TSD: append this round's record. Best-effort — an
// unbuilt or missing tsd-write binary means the row stays unwritten, never that the round
// itself fails.
// ---------------------------------------------------------------------------------------

/// `test-registry deny-list --at <at>` by bare name on the launcher's PATH: the text a round
/// hands to every pass. A registry that does not answer is an Err naming what it said.
pub fn deny_list_snapshot(env: &Env, at: u64) -> Result<String, String> {
    deny_list_from(&spira_config::release_env::tool("test-registry")?, env, at)
}

pub fn deny_list_from(bin: &str, env: &Env, at: u64) -> Result<String, String> {
    // batch-job: a registry read, bounded by the registry's own busy timeout
    // FORK-EDGE: machine leg test-registry (deny-list snapshot at the cut)
    let out = Command::new(bin)
        .args(["deny-list", "--at", &at.to_string()])
        .env("SPIRA_HOME", &env.home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{bin} deny-list: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(format!(
        "{bin} deny-list --at {at} exited {}: {}{}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

pub fn tsd_append_round(env: &Env, fields: &[(&str, String)]) {
    tsd_append(env, "batch-round", fields)
}

/// One row of `family` through tsd-write; best-effort, like every TSD write here.
pub fn tsd_append(env: &Env, family: &str, fields: &[(&str, String)]) {
    let Some(bin) = &env.tsd_bin else { return };
    // FORK-EDGE: needs tsd-write's family append as a tsd library call
    let mut cmd = spira_config::bounded::bounded(bin);
    cmd.arg("--family").arg(family).arg("--root").arg(&env.run);
    for (k, v) in fields {
        cmd.arg("--field-str").arg(format!("{k}={v}"));
    }
    let _ = cmd.status();
}

// ---------------------------------------------------------------------------------------
// Judgement: file a bead through bead.sh's own contract (never `bd create` directly) so the
// batcher persona's partition labels come from its rune, the same way every other filed
// bead in this harness does (sp-47kq1).
// ---------------------------------------------------------------------------------------

/// File a judgement bead for the called batcher persona and return its id. The body is
/// written to a scratch file under `env.run/tmp` rather than passed inline, matching every
/// other `--body-file` caller in this harness (arbitrary suite output as a shell argument is
/// how a stray backtick becomes command substitution).
pub fn file_judgement(
    env: &Env,
    repo: &Repo,
    j: &batcher::core::Judgement,
    members: &[String],
    evidence: &str,
    red_output: Option<&Path>,
) -> Result<String, String> {
    use batcher::core::{judgement_body, RedSource};
    let title = format!(
        "batcher: {} double-red in {} needs judgement ({})",
        match j.source {
            RedSource::Local => "local",
            RedSource::Ci => "CI",
        },
        repo.name,
        j.suites.join(",")
    );
    let body = format!("{}{}\n", judgement_body(&repo.name, j, members, evidence), red_output_line(red_output));
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("judgement-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        // FORK-EDGE: bead.sh is the operator tool for filing; it stays
        Command::new("bead.sh")
            .arg("file")
            .arg(&title)
            .arg("--for")
            .arg("batcher")
            .arg("--repo")
            .arg(&repo.name)
            .arg("--harness-title")
            .arg("--body-file")
            .arg(&tmp)
            .arg("--json"),
        "bead.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    // THE ID COMES FROM PARSING THE JSON, not scanning human output for an id-shaped token
    // (law-never-derive-an-id-from-output): `bd create` without --silent prints an advisory
    // that echoes the title before the id, and this title itself names suites and a repo.
    gate::flip::bead_id(&out)
}

/// File the follow-up bead for a flipped test that the round disabled and return its id.
pub fn file_flip_bead(
    env: &Env,
    repo: &Repo,
    test: &str,
    round_branch: &str,
    red_output: Option<&Path>,
) -> Result<String, String> {
    let title = format!(
        "{}: {test} flipped in a batcher round and was disabled — fix or delete it",
        repo.name
    );
    let body = flip_body(test, round_branch, red_output);
    file_batcher_bead(env, repo, "flip", &title, &body)
}

/// The body of a flipped test's follow-up bead.
pub fn flip_body(test: &str, round_branch: &str, red_output: Option<&Path>) -> String {
    format!(
        "{test} was red on the round tree ({round_branch}) and green on a rerun of that unchanged tree, on the base, and on every member alone, so the batcher disabled it in the test registry for the registry's hold (spira.test_registry_hold_secs) and certified the round without it (law-a-test-that-flips-is-deleted). It runs again when the row expires.\n\nDeliverable: find the nondeterminism, then fix or delete the test; a deterministic test restores coverage parity.\n\n{}\n",
        red_output_line(red_output)
    )
}

/// File the fix bead for a test that is red on the merged tree and on the base after its owner lifted its row.
pub fn file_kept_row_bead(
    env: &Env,
    repo: &Repo,
    kept: &KeptRow,
) -> Result<String, String> {
    let title = format!("{} red on the merged tree and the base after {} lifted its row — fix it", kept.test, kept.owner);
    let lines = |ls: &[String]| if ls.is_empty() { "(not captured)".to_string() } else { ls.join("\n") };
    let body = format!(
        "{test} was red on the round tree and on the base, so {owner}'s landing did not clear its registry row: the row stays, now pointed at this bead (it was {old}).\n\nRound branch: {branch}\nMerged head: {head}\nBase: {base}\n\nFailing on the merged tree:\n{merged}\n\nFailing on the base:\n{base_lines}\n\nDeliverable: make {test} green on the base; its row then expires or is cleared by this bead's landing.\n",
        test = kept.test,
        owner = kept.owner,
        old = kept.old_bead,
        branch = kept.round_branch,
        head = kept.head,
        base = kept.base,
        merged = lines(&kept.merged_lines),
        base_lines = lines(&kept.base_lines),
    );
    file_batcher_bead(env, repo, "kept", &title, &body)
}

/// What the bead filed for a kept row says.
pub struct KeptRow {
    pub test: String,
    pub owner: String,
    pub old_bead: String,
    pub round_branch: String,
    pub head: String,
    pub base: String,
    pub merged_lines: Vec<String>,
    pub base_lines: Vec<String>,
}

fn file_batcher_bead(env: &Env, repo: &Repo, tag: &str, title: &str, body: &str) -> Result<String, String> {
    gate::flip::file_bead(&env.run.join("tmp"), &repo.name, "batcher", tag, title, body)
}

pub use gate::flip::{parse_disabled_row, DisabledRow};

/// `test-registry disable` of `test` for the registry's own hold, as the batcher.
pub fn registry_disable(bin: &str, env: &Env, test: &str, bead: &str, reason: &str) -> Result<DisabledRow, String> {
    gate::flip::registry_disable(bin, &env.home, test, bead, reason, "batcher")
}

/// `registry_disable` as the closure `disable_flips_with` takes.
pub fn through_registry<'a>(bin: &'a str, env: &'a Env) -> impl FnMut(&str, &str, &str) -> Result<DisabledRow, String> + 'a {
    move |test, bead, reason| registry_disable(bin, env, test, bead, reason)
}

/// Disables each flipped test: files its follow-on bead, writes its registry row, and adds the
/// row to the deny list the round's next pass reads from `results`. Stops at the first failure.
pub fn disable_flips_with(
    file_bead: &mut dyn FnMut(&str) -> Result<String, String>,
    disable: &mut dyn FnMut(&str, &str, &str) -> Result<DisabledRow, String>,
    results: &Path,
    round_branch: &str,
    tests: &[String],
) -> Result<Vec<DisabledRow>, String> {
    let mut rows = vec![];
    for id in tests {
        let bead = file_bead(id).map_err(|e| format!("cannot file the follow-up for flipped {id}: {e}"))?;
        let reason = format!("{id} flipped in {round_branch} (red on the round tree, green rerun unchanged, green on the base and on every member alone)");
        let row = disable(id, &bead, &reason)
            .map_err(|e| format!("cannot disable flipped {id} (follow-up {bead}): {e}"))?;
        let list = crate::vm::DenySnapshot::from_pass(results)
            .ok_or_else(|| format!("{} holds no deny list to add {id} to", results.display()))?;
        let path = results.join(crate::vm::DenySnapshot::FILE);
        let next = list.with_row(&row.state, &row.test, &row.bead, row.until);
        fs::write(&path, &next.text).map_err(|e| format!("{}: {e}", path.display()))?;
        rows.push(row);
    }
    Ok(rows)
}

/// Adds `row` to the deny list a pass of the round reads from `results`.
pub fn add_row_to_pass(results: &Path, row: &crate::vm::LiftedRow) -> Result<(), String> {
    let list = crate::vm::DenySnapshot::from_pass(results)
        .ok_or_else(|| format!("{} holds no deny list to add {} to", results.display(), row.test))?;
    let path = results.join(crate::vm::DenySnapshot::FILE);
    let next = list.with_row(&row.state, &row.test, &row.bead, row.until);
    fs::write(&path, &next.text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The `test:<id>` rows among `rows` that `tip` claims against `base_sha`: its diff changes the
/// body of the test the row names.
pub fn member_claims(repo: &Repo, base_sha: &str, tip: &str, rows: &[String]) -> Vec<String> {
    if rows.is_empty() {
        return vec![];
    }
    let paths: Vec<String> = changed_paths_checked(repo, base_sha, tip).unwrap_or_default().into_iter().filter(|p| p.ends_with(".rs")).collect();
    let read = |rev: &str| -> Vec<(String, Vec<u8>)> { paths.iter().filter_map(|p| file_at(repo, rev, p).map(|t| (p.clone(), t.into_bytes()))).collect() };
    crate::claim::claimed_rows(rows, &read(base_sha), &read(tip))
}

/// `test-registry activate` of `test` as the batcher.
pub fn registry_activate(bin: &str, env: &Env, test: &str, reason: &str) -> Result<(), String> {
    // batch-job: a registry write, bounded by the registry's own busy timeout
    // FORK-EDGE: machine leg test-registry (activate a claimed row the round landed green)
    let out = Command::new(bin)
        .args(["activate", "--test", test, "--reason", reason, "--actor", "batcher"])
        .env("SPIRA_HOME", &env.home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{bin} activate: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{bin} activate --test {test} exited {}: {}{}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Clears each claimed row a landed round left standing: one `activate` per row. Returns the
/// rows it could not clear, with why; a failed clear never undoes the landing.
pub fn activate_claimed_with(activate: &mut dyn FnMut(&str, &str) -> Result<(), String>, round: &str, claims: &[(String, String)]) -> Vec<(String, String)> {
    claims
        .iter()
        .filter_map(|(test, member)| {
            let reason = format!("body changed by {member}; green in round {round}");
            activate(test, &reason).err().map(|e| (test.clone(), e))
        })
        .collect()
}

/// The registry rows a round lifted, and what became of each: kept (re-pointed at a fix bead) or
/// put back because their owner left.
#[derive(Default)]
pub struct LiftedRows {
    pub rows: Vec<crate::vm::LiftedRow>,
    kept: std::collections::BTreeMap<String, String>,
    restored: std::collections::BTreeSet<String>,
}

impl LiftedRows {
    pub fn new(rows: Vec<crate::vm::LiftedRow>) -> Self {
        LiftedRows { rows, ..Default::default() }
    }

    /// (test, owner) of each row still lifted: not kept and not put back.
    pub fn standing(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .filter(|r| !self.kept.contains_key(&r.test) && !self.restored.contains(&r.test))
            .map(|r| (r.test.clone(), r.claimed_by.clone().unwrap_or_else(|| r.bead.clone())))
            .collect()
    }

    /// (test, member) of each claimed row still lifted: the rows a landing activates.
    pub fn standing_claims(&self) -> Vec<(String, String)> {
        let standing = self.standing();
        self.rows
            .iter()
            .filter(|r| standing.iter().any(|(t, _)| *t == r.test))
            .filter_map(|r| Some((r.test.clone(), r.claimed_by.clone()?)))
            .collect()
    }

    /// Keeps `test` once per round: the second call answers the first one's bead and files nothing.
    pub fn keep(&mut self, test: &str, keep: impl FnOnce(&str) -> Result<String, String>) -> Result<String, String> {
        if let Some(bead) = self.kept.get(test) {
            return Ok(bead.clone());
        }
        let old = self.rows.iter().find(|r| r.test == test).map(|r| r.bead.clone()).ok_or_else(|| format!("{test} is not a row this round lifted"))?;
        let bead = keep(&old)?;
        self.kept.insert(test.to_string(), bead.clone());
        Ok(bead)
    }

    /// Puts `owner`'s lifted rows back in the pass's deny list, except those already kept.
    pub fn restore_rows_of(&mut self, owner: &str, results: &Path) -> Result<(), String> {
        for row in self.rows.iter().filter(|r| r.claimed_by.as_deref().unwrap_or(&r.bead) == owner) {
            if self.kept.contains_key(&row.test) || !self.restored.insert(row.test.clone()) {
                continue;
            }
            add_row_to_pass(results, row)?;
        }
        Ok(())
    }
}

/// Keeps a lifted row that is red on the merged tree and the base: files its fix bead, re-points
/// the registry row at it and puts the row back in the pass's deny list. Stops at the first failure.
pub fn keep_row_with(
    file_bead: &mut dyn FnMut(&KeptRow) -> Result<String, String>,
    disable: &mut dyn FnMut(&str, &str, &str) -> Result<DisabledRow, String>,
    results: &Path,
    kept: &KeptRow,
) -> Result<DisabledRow, String> {
    let bead = file_bead(kept).map_err(|e| format!("cannot file the fix for {}: {e}", kept.test))?;
    let reason = format!(
        "kept: {} red on {} and on the base {}; {}'s landing did not clear it; re-pointed from {} to {bead}",
        kept.test, kept.round_branch, kept.base, kept.owner, kept.old_bead
    );
    let row = disable(&kept.test, &bead, &reason).map_err(|e| format!("cannot re-point {} (fix {bead}): {e}", kept.test))?;
    add_row_to_pass(
        results,
        &crate::vm::LiftedRow { state: row.state.clone(), test: row.test.clone(), bead: row.bead.clone(), until: row.until, claimed_by: None },
    )?;
    Ok(row)
}

/// The local calendar date of an epoch second.
pub fn date_of(epoch: i64) -> String {
    // EXTERNAL: date — the local calendar date; std has no local time zone
    run(Command::new("date").arg("-d").arg(format!("@{epoch}")).arg("+%F"), "date")
        .map(|d| d.trim().to_string())
        .unwrap_or_else(|_| epoch.to_string())
}

// ---------------------------------------------------------------------------------------
// Locking: one round-cut at a time per repo, same lock file batch.sh itself used.
// ---------------------------------------------------------------------------------------

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

pub struct Lock {
    file: fs::File,
    beat: Option<Beat>,
}

const BEAT_FILE: &str = "lock.beat";
const BEAT_EVERY: std::time::Duration = std::time::Duration::from_secs(20);

/// Touches `lock.beat` while the queue-lock holder lives, so the reconciler tells a landing that
/// is still working from a holder that died with the lock held (the queue crate's holder does the same).
struct Beat {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

fn touch(path: &Path) {
    if let Ok(f) = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
    {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

impl Beat {
    fn start(path: PathBuf, every: std::time::Duration) -> Beat {
        touch(&path);
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let p = path.clone();
        let thread = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(every) {
                touch(&p);
            }
        });
        Beat {
            stop,
            thread: Some(thread),
            path,
        }
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        self.beat.take();
    }
}

fn lock_at(path: &Path, verb: &str) -> Result<Option<Lock>, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return Ok(None);
    }
    let _ = f.set_len(0);
    let _ = (&f).write_all(format!("pid={} verb={verb}\n", std::process::id()).as_bytes());
    let beat = path
        .file_name()
        .is_some_and(|n| n == "lock")
        .then(|| path.with_file_name(BEAT_FILE))
        .map(|b| Beat::start(b, BEAT_EVERY));
    Ok(Some(Lock { file: f, beat }))
}

/// None means another operation already holds the lock — not an error, the caller should
/// simply skip this pass, same as batch.sh's own `flock -n` behaviour. The holder is recorded
/// in the lock file so a waiter can name it.
pub fn try_lock(env: &Env, repo: &str) -> Result<Option<Lock>, String> {
    lock_at(&env.queue_dir.join(repo).join("lock"), "batcher")
}

thread_local! {
    static HELD_QUEUE_LOCK: std::cell::RefCell<Option<Lock>> = const { std::cell::RefCell::new(None) };
}

/// Parks the queue lock a cut took where `unlocked` can release it around a long leg; dropping
/// the guard releases whatever is still parked.
pub struct Parked;

pub fn park_queue_lock(lock: Lock) -> Parked {
    HELD_QUEUE_LOCK.with(|s| *s.borrow_mut() = Some(lock));
    Parked
}

impl Drop for Parked {
    fn drop(&mut self) {
        HELD_QUEUE_LOCK.with(|s| s.borrow_mut().take());
    }
}

/// Runs a long leg (a suite run) without the queue lock, so an operator verb is not refused for
/// its length, and marks `batch` driven by holding its certify lock meanwhile. The queue lock
/// is taken again before returning: state transitions stay under it.
pub fn unlocked<T>(
    env: &Env,
    repo: &str,
    batch: Option<&str>,
    wait_secs: u64,
    leg: impl FnOnce() -> T,
) -> Result<T, String> {
    let Some(batch) = batch else { return Ok(leg()) };
    let held = HELD_QUEUE_LOCK.with(|s| s.borrow_mut().take()).is_some();
    let driving = if held {
        let marker = drive_marker(env, repo, batch);
        if marker.is_none() {
            return Err(format!(
                "{repo}: a leg may not run unlocked without marking {batch} driven"
            ));
        }
        marker
    } else {
        None
    };
    let out = leg();
    drop(driving);
    if held {
        let l = loop {
            if let Some(l) = wait_lock(env, repo, wait_secs)? {
                break l;
            }
            eprintln!("{repo}: another operation holds the queue lock after the leg (waited {wait_secs}s) — keeping the leg's result, retrying");
        };
        HELD_QUEUE_LOCK.with(|s| *s.borrow_mut() = Some(l));
    }
    Ok(out)
}

fn drive_marker(env: &Env, repo: &str, batch: &str) -> Option<fs::File> {
    let path = env
        .queue_dir
        .join(repo)
        .join(format!("certify-{batch}.lock"));
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    if unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return None;
    }
    let _ = f.set_len(0);
    let _ = (&f).write_all(format!("pid={} verb=batcher cut\n", std::process::id()).as_bytes());
    Some(f)
}

/// Whether a `round certify` of `batch` is running, asked of the lock it holds for its whole run.
pub fn certify_driven(env: &Env, repo: &str, batch: &str) -> bool {
    let path = env
        .queue_dir
        .join(repo)
        .join(format!("certify-{batch}.lock"));
    if !path.exists() {
        return false;
    }
    let Ok(f) = fs::OpenOptions::new().write(true).open(&path) else {
        return false;
    };
    unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) != 0 }
}

/// Like `try_lock` but polls for up to `wait_secs`, for the hand path: a routine sweep's
/// few-second hold must not make a single invocation silently do nothing.
pub fn wait_lock(env: &Env, repo: &str, wait_secs: u64) -> Result<Option<Lock>, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    loop {
        if let Some(l) = try_lock(env, repo)? {
            return Ok(Some(l));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod land_tests {
    use super::*;

    #[test_id(t01lexs)]
    #[test]
    fn a_full_host_filesystem_files_its_incident_under_the_host_disk_cause() {
        assert_eq!(local_red_cause("host-disk"), "host-disk");
        assert_eq!(local_red_cause("unattributed"), "local-round-red");
    }

    #[test_id(tdlfbv2)]
    #[test]
    fn foreign_in_names_only_non_members() {
        let subj = |s: &str| s.to_string();
        let subjects = vec![
            subj("spira: land sp-lc1puh"),
            subj("sp-lc1puh: parallel ui tier"),
            subj("spira: land sp-94a87q"),
            subj("sp-ffh07b.2: child work"),
            subj("Merge branch 'x' into y"),
        ];
        let members = vec!["sp-94a87q".to_string(), "sp-ffh07b.2".to_string()];
        assert_eq!(
            foreign_in(&subjects, &members),
            vec!["sp-lc1puh".to_string()]
        );
        assert!(foreign_in(&subjects[2..4], &members).is_empty());
    }
}
#[cfg(test)]
mod pool_parity_tests {
    use super::*;

    /// law-lifecycle-state-is-not-text (sp-mijstw): a busy lock is retried on queue's exit code;
    /// a final refusal whose words mention a lock is not retried, and a busy exit is retried
    /// whatever its words. A final refusal misread as busy re-ran it; a busy lock misread as
    /// final abandoned the round as land-refused.
    #[test_id(t9svmkb)]
    #[test]
    fn a_busy_lock_is_its_exit_code_never_its_words() {
        assert!(lock_busy(Some(QUEUE_LOCK_BUSY_EXIT)));
        assert!(!lock_busy(Some(1)));
        assert!(!lock_busy(Some(0)));
        assert!(!lock_busy(None));
    }

    #[test_id(ts535y8)]
    #[test]
    fn an_eject_refused_for_a_running_pass_is_held_with_what_it_takes_to_issue_it_again() {
        let run = |code| VerbRun { code: Some(code), out: String::new(), err: "queue.sh round eject: a pass of round b is running (x.scope)\n".into() };
        let held = eject_result(&run(QUEUE_PASS_RUNNING_EXIT), "sp-a", "why\nit", "s1,s2").unwrap_err();
        let h = HeldEject::parse(&format!("eject of sp-a refused: {held}")).expect("held");
        assert_eq!((h.id.as_str(), h.reason.as_str(), h.csv.as_str()), ("sp-a", "why\nit", "s1,s2"));
        assert!(h.said.contains("a pass of round b is running"), "{}", h.said);
        let other = eject_result(&run(1), "sp-a", "r", "").unwrap_err();
        assert!(HeldEject::parse(&other).is_none() && !other.contains(EJECT_HELD), "{other}");
        assert!(eject_result(&run(0), "sp-a", "r", "").is_ok());
    }

    #[test_id(tou71if)]
    #[test]
    fn the_pool_takes_a_submitted_unheld_bead_as_provisional_and_a_certified_one_as_not() {
        let rows = vec![
            serde_json::json!({"bead_id": "sp-s", "state": "SUBMITTED", "tip": "ts", "holds": []}),
            serde_json::json!({"bead_id": "sp-c", "state": "CERTIFIED", "tip": "tc", "holds": []}),
        ];
        let pool = pool_rows(&rows, "spira", Land::Local).0;
        let flags: Vec<(&str, bool)> = pool.iter().map(|p| (p.0.as_str(), p.5)).collect();
        assert_eq!(flags, [("sp-c", false), ("sp-s", true)]);
    }

    #[test_id(tn1p4ut)]
    #[test]
    fn the_forge_pool_takes_a_submitted_bead_only_when_its_gate_certifies_nothing() {
        let rows = vec![
            serde_json::json!({"bead_id": "sp-u", "state": "SUBMITTED", "tip": "tu", "holds": []}),
            serde_json::json!({"bead_id": "sp-n", "state": "SUBMITTED", "tip": "tn", "holds": [], "reason": "gate-certifies-nothing"}),
            serde_json::json!({"bead_id": "sp-c", "state": "CERTIFIED", "tip": "tc", "holds": []}),
        ];
        let (pool, left) = pool_rows(&rows, "spira", Land::Forge);
        assert_eq!(pool.iter().map(|p| (p.0.as_str(), p.5)).collect::<Vec<_>>(), [("sp-c", false), ("sp-n", true)]);
        assert_eq!(left, [LeftOut { id: "sp-u".into(), tip: "tu".into(), cause: Cause::Uncertified }]);
        assert!(left[0].line("spira").contains("not certified"), "{}", left[0].line("spira"));
        let (pool, left) = pool_rows(&rows, "spira", Land::Local);
        assert_eq!(pool.iter().map(|p| (p.0.as_str(), p.5)).collect::<Vec<_>>(), [("sp-c", false), ("sp-n", true), ("sp-u", true)]);
        assert!(left.is_empty(), "{left:?}");
    }

    #[test_id(tw514ey)]
    #[test]
    fn a_member_stacked_on_an_uncertified_or_moved_row_is_left_out_with_it() {
        let rows = [serde_json::json!({"bead_id": "sp-u", "state": "SUBMITTED", "tip": "tu", "holds": []})];
        let mut left = pool_rows(&rows, "spira", Land::Forge).1;
        left.push(LeftOut { id: "sp-m".into(), tip: "tm".into(), cause: Cause::TipMoved { branch_tip: "new".into() } });
        left.push(LeftOut { id: "sp-w".into(), tip: "tw".into(), cause: Cause::NoContentRow });
        let member = |id: &str, on: &str| Member { id: id.into(), tip: format!("t-{id}"), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::from([(on.to_string(), "t".to_string())]), blocked_by: vec![], provisional: false };
        let (kept, dropped) = drop_stacked_on_barred(vec![member("sp-a", "sp-u"), member("sp-b", "sp-m"), member("sp-c", "sp-w")], barred_by_left(&left));
        assert_eq!(kept.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["sp-c"]);
        assert_eq!(ids_of(&dropped), ["sp-a", "sp-b"]);
    }

    fn gate_own_pool(rows: &[serde_json::Value], changed: &[(&str, &[&str])]) -> (Vec<PoolRow>, Vec<LeftOut>) {
        let own = gate::def::parse("own gate.steps\nown gate/\nstep a\n").unwrap();
        let changed = changed.iter().map(|(id, ps)| (id.to_string(), ps.iter().map(|p| p.to_string()).collect())).collect();
        gate_own_left_out(pool_rows(rows, "spira", Land::Local).0, &own, &changed)
    }

    #[test_id(tl062tz)]
    #[test]
    fn a_submitted_item_touching_a_gate_own_file_is_left_out_naming_the_paths() {
        let row = serde_json::json!({"bead_id": "sp-g", "state": "SUBMITTED", "tip": "tg", "holds": []});
        let (pool, left) = gate_own_pool(std::slice::from_ref(&row), &[("sp-g", &["a.rs", "gate.steps"])]);
        assert!(pool.is_empty());
        assert_eq!(left, [LeftOut { id: "sp-g".into(), tip: "tg".into(), cause: Cause::GateUnjudged { paths: vec!["gate.steps".into()] } }]);
        let (pool, left) = gate_own_pool(&[row], &[("sp-g", &["gate/src/fence.rs"])]);
        assert!(pool.is_empty());
        assert_eq!(left[0].cause, Cause::GateUnjudged { paths: vec!["gate/src/fence.rs".into()] });
    }

    #[test_id(tgkorkl)]
    #[test]
    fn the_same_item_certified_at_its_tip_is_cut() {
        let row = serde_json::json!({"bead_id": "sp-g", "state": "CERTIFIED", "tip": "tg", "holds": []});
        let (pool, left) = gate_own_pool(&[row], &[("sp-g", &["gate.steps"])]);
        assert!(left.is_empty());
        assert_eq!(pool.iter().map(|p| (p.0.as_str(), p.5)).collect::<Vec<_>>(), [("sp-g", false)]);
    }

    #[test_id(tfsco9o)]
    #[test]
    fn a_submitted_item_touching_no_gate_own_file_is_cut_provisional_as_today() {
        let plain = serde_json::json!({"bead_id": "sp-o", "state": "SUBMITTED", "tip": "to", "holds": []});
        let express = serde_json::json!({"bead_id": "sp-x", "state": "SUBMITTED", "tip": "tx", "holds": [], "express": true});
        let express_gate = serde_json::json!({"bead_id": "sp-y", "state": "SUBMITTED", "tip": "ty", "holds": [], "express": true});
        let (pool, left) = gate_own_pool(
            &[plain, express, express_gate],
            &[("sp-o", &["spira-lc/src/work.rs"]), ("sp-x", &["spira-lc/src/work.rs"]), ("sp-y", &["gate.steps"])],
        );
        assert_eq!(pool.iter().map(|p| (p.0.as_str(), p.5)).collect::<Vec<_>>(), [("sp-o", true), ("sp-x", true)]);
        assert_eq!(left.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["sp-y"]);
    }

    #[test_id(t7mjel7)]
    #[test]
    fn an_unreadable_gate_definition_or_diff_leaves_out_provisional_rows_and_keeps_certified_ones() {
        let rows = [
            serde_json::json!({"bead_id": "sp-p", "state": "SUBMITTED", "tip": "tp", "holds": []}),
            serde_json::json!({"bead_id": "sp-c", "state": "CERTIFIED", "tip": "tc", "holds": []}),
        ];
        let pool = || pool_rows(&rows, "spira", Land::Local).0;
        let ids = |p: &[PoolRow]| p.iter().map(|r| r.0.clone()).collect::<Vec<_>>();
        let (kept, left) = gate_own_apply(pool(), Some("nonsense\n"), &mut |_| Ok(vec![]));
        assert_eq!((ids(&kept), ids_of(&left)), (vec!["sp-c".to_string()], vec!["sp-p".to_string()]));
        let (kept, left) = gate_own_apply(pool(), Some("own gate/\nstep a\n"), &mut |_| Err("boom".into()));
        assert_eq!((ids(&kept), ids_of(&left)), (vec!["sp-c".to_string()], vec!["sp-p".to_string()]));
        let (kept, left) = gate_own_apply(pool(), None, &mut |_| panic!("an ungated repo reads no diff"));
        assert_eq!((kept.len(), left.len()), (2, 0));
        let mut asked = Vec::new();
        gate_own_apply(pool(), Some("own gate/\nstep a\n"), &mut |r| { asked.push(r.0.clone()); Ok(vec![]) });
        assert_eq!(asked, ["sp-p"]);
    }

    fn ids_of(left: &[LeftOut]) -> Vec<String> {
        left.iter().map(|l| l.id.clone()).collect()
    }

    #[test_id(tj6w7pm)]
    #[test]
    fn a_gate_unjudged_item_is_noted_once_per_tip() {
        let run = testkit::TempDir::new("batcher-cut-gate-unjudged");
        let at = |tip: &str| [LeftOut { id: "sp-g".into(), tip: tip.into(), cause: Cause::GateUnjudged { paths: vec!["gate.steps".into()] } }];
        let announce = |left: &[LeftOut]| announce_with(run.path(), "r", left, &mut |_, _| Ok(()));
        let first = announce(&at("t1"));
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("gate.steps") && first[0].contains("CERTIFIED"), "{first:?}");
        assert!(announce(&at("t1")).is_empty());
        assert_eq!(announce(&at("t2")).len(), 1);
    }

    #[test_id(tikkw37)]
    #[test]
    fn a_submitted_bead_held_or_at_its_ejected_red_tip_is_left_out_naming_its_cause() {
        let held = |kind: &str| serde_json::json!({"bead_id": "sp-h", "state": "SUBMITTED", "tip": "th", "holds": [kind]});
        for kind in ["manual", "wait", "ask", "poison"] {
            let (pool, left) = pool_rows(&[held(kind)], "spira", Land::Local);
            assert!(pool.is_empty(), "{kind}");
            assert_eq!(left[0].cause, Cause::Held(vec![kind.into()]));
        }
        let red = serde_json::json!({"bead_id": "sp-e", "state": "SUBMITTED", "tip": "te", "holds": [], "ejected_red_tip": "te"});
        let (pool, left) = pool_rows(&[red], "spira", Land::Local);
        assert!(pool.is_empty());
        assert_eq!(left[0].cause, Cause::EjectedRed);
    }

    #[test_id(t6ohgxl)]
    #[test]
    fn another_repos_rows_never_enter_this_repos_pool_or_its_left_out_list() {
        let row = |id: &str, repo: &str, state: &str, holds: &[&str]| serde_json::json!({"bead_id": id, "repo": repo, "state": state, "tip": "t", "holds": holds});
        let rows = [
            row("sp-sh", "spira", "SUBMITTED", &["manual"]),
            row("sp-sc", "spira", "CERTIFIED", &[]),
            row("cb-h", "cubby", "SUBMITTED", &["manual"]),
            row("cb-c", "cubby", "CERTIFIED", &[]),
        ];
        let (pool, left) = pool_rows(&rows, "spira", Land::Local);
        assert_eq!(pool.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["sp-sc"]);
        assert_eq!(left.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["sp-sh"]);
        let branches = BTreeMap::from([("sp-sc".to_string(), "t".to_string())]);
        assert!(moved_tips(&pool, &branches).is_empty());
    }

    #[test_id(t7vx0o2)]
    #[test]
    fn only_an_in_delivery_red_mark_that_bars_the_tip_and_follows_no_gate_pass_is_ejectable() {
        let row = |reason: &str, gate_key: serde_json::Value| serde_json::json!({"state": "IN_DELIVERY", "tip": "t", "reason": reason, "gate_key": gate_key});
        let null = serde_json::Value::Null;
        assert_eq!(gate_red_of("sp-a", &row("gate-red: suites-failed", null.clone())), Some(("sp-a".into(), "t".into(), "suites-failed".into())));
        assert_eq!(gate_red_of("sp-a", &row("gate-red: timeout", null.clone())), None, "a killed run says nothing about the tree");
        assert_eq!(gate_red_of("sp-a", &row("gate-red: no-rebase", null.clone())), None);
        assert_eq!(gate_red_of("sp-a", &row("gate-red: suites-failed", "round:k".into())), None, "a member whose gate passed is never preempted on a later mark");
        assert_eq!(gate_red_of("sp-a", &row("delivered-from-submitted", null)), None);
    }

    #[test_id(tolbuup)]
    #[test]
    fn a_blocked_bead_is_named_with_its_dependency_once_per_tip() {
        let run = testkit::TempDir::new("batcher-cut-blocked");
        let left = [LeftOut { id: "sp-c".into(), tip: "tc".into(), cause: Cause::Blocked { dep: "sp-p".into() } }];
        let announce = |left: &[LeftOut]| announce_with(run.path(), "r", left, &mut |_, _| Ok(()));
        let first = announce(&left);
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("sp-c") && first[0].contains("sp-p"), "{first:?}");
        assert_eq!(fs::read_to_string(left_out_dir(run.path(), "r").join("sp-c")).unwrap(), "blocked tc\n");
        assert!(announce(&left).is_empty());
    }

    #[test_id(t6d9dch)]
    #[test]
    fn the_cut_names_every_bead_it_refuses_with_its_own_cause() {
        let pool_left = vec![
            LeftOut { id: "sp-s".into(), tip: "ts".into(), cause: Cause::StackedOn { prereq: "sp-h".into() } },
            LeftOut { id: "sp-n".into(), tip: "tn".into(), cause: Cause::NoContentRow },
        ];
        let tips: BTreeMap<String, String> = [("sp-c".to_string(), "tc".to_string())].into();
        let refusals = [batcher::core::BlockedRefusal { id: "sp-c".into(), blocker: "sp-p".into() }];
        let left = cut_left_out(pool_left, &tips, &refusals);
        let causes: Vec<_> = left.iter().map(|l| (l.id.as_str(), l.tip.as_str(), l.cause.clone())).collect();
        assert_eq!(
            causes,
            [
                ("sp-s", "ts", Cause::StackedOn { prereq: "sp-h".into() }),
                ("sp-n", "tn", Cause::NoContentRow),
                ("sp-c", "tc", Cause::Blocked { dep: "sp-p".into() }),
            ]
        );
    }

    #[test_id(to522kc)]
    #[test]
    fn the_pool_takes_a_certified_unheld_bead() {
        let rows = vec![
            serde_json::json!({"bead_id": "sp-c", "state": "CERTIFIED", "tip": "tc", "holds": []}),
        ];
        let pool = pool_rows(&rows, "spira", Land::Local).0;
        assert_eq!(pool.len(), 1);
        assert_eq!((pool[0].0.as_str(), pool[0].1.as_str()), ("sp-c", "tc"));
    }

    #[test_id(t44ky6q)]
    #[test]
    fn a_certified_bead_the_pool_leaves_out_is_named_once_per_cause() {
        let rows = vec![
            serde_json::json!({"bead_id": "sp-p", "state": "CERTIFIED", "tip": "tp", "holds": []}),
            serde_json::json!({"bead_id": "sp-h", "state": "CERTIFIED", "tip": "th", "holds": ["manual"]}),
            serde_json::json!({"bead_id": "sp-e", "state": "CERTIFIED", "tip": "te", "holds": [], "ejected_red_tip": "te"}),
        ];
        let (pool, left) = pool_rows(&rows, "spira", Land::Local);
        assert_eq!(pool.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["sp-p"]);
        let causes: Vec<_> = left.iter().map(|l| (l.id.as_str(), l.cause.clone())).collect();
        assert_eq!(causes, [("sp-h", Cause::Held(vec!["manual".into()])), ("sp-e", Cause::EjectedRed)]);

        let run = testkit::TempDir::new("batcher-cut-left-out");
        let noted = std::cell::RefCell::new(Vec::new());
        let announce = |left: &[LeftOut]| {
            announce_with(run.path(), "r", left, &mut |id, _| {
                noted.borrow_mut().push(id.to_string());
                Ok(())
            })
        };
        assert_eq!(announce(&left).len(), 2);
        assert!(announce(&left).is_empty(), "an unchanged cause is named once");
        let mut moved = left.clone();
        moved[1].tip = "te2".into();
        assert_eq!(announce(&moved).len(), 1, "a changed tip is named again");
        moved[0].cause = Cause::TipMoved { branch_tip: "bt".into() };
        let lines = announce(&moved);
        assert!(lines.len() == 1 && lines[0].contains("spira/sp-h at bt"), "{lines:?}");
        assert!(announce(&moved[..1]).is_empty());
        assert!(!left_out_dir(run.path(), "r").join("sp-e").exists(), "a bead back in the pool loses its record");
        assert_eq!(announce(&left).len(), 2, "and is named again if it is left out again");

        let failing = announce_with(run.path(), "r", &left[..1].iter().cloned().map(|mut l| { l.id = "sp-n".into(); l }).collect::<Vec<_>>(), &mut |_, _| Err("down".into()));
        assert_eq!(failing.len(), 1);
        assert_eq!(
            announce_with(run.path(), "r", &[LeftOut { id: "sp-n".into(), ..left[0].clone() }], &mut |_, _| Ok(())).len(),
            1,
            "a failed note leaves no record, so the line comes back"
        );
    }

    #[test_id(tbgxy5x)]
    #[test]
    fn each_cut_cause_line_names_what_readmits_the_bead() {
        let causes = [
            (Cause::Refused { kind: "integration".into() }, "refused-integration", "(integration)", "a new tip"),
            (Cause::NoSift, "no-sift", "no sift pass", "a sift pass at t1"),
            (Cause::RoundFull { cap: 7 }, "round-full", "full at 7", "a later round"),
            (Cause::OutsideFeature { root: "sp-f".into() }, "outside-feature", "feature sp-f", "the next cut"),
        ];
        for (cause, tag, why, exit) in causes {
            let l = LeftOut { id: "sp-a".into(), tip: "t1".into(), cause };
            let line = l.line("r");
            assert_eq!(l.tag(), tag);
            let (head, tail) = line.split_once(" — ").unwrap_or_else(|| panic!("no exit in {line}"));
            assert!(head.starts_with("batcher r: sp-a left out of the cut: ") && head.contains(why), "{line}");
            assert!(tail.contains(exit), "{line}");
        }
    }

    #[test_id(tfd6ogn)]
    #[test]
    fn a_certified_tip_the_branch_moved_past_is_left_out_naming_the_branch_tip() {
        let row = |id: &str, tip: &str| (id.to_string(), tip.to_string(), 0, false, vec![], false);
        let branches = BTreeMap::from([("sp-m".to_string(), "new".to_string()), ("sp-s".to_string(), "same".to_string())]);
        let pool = [row("sp-m", "old"), row("sp-s", "same"), row("sp-x", "gone")];
        let left = moved_tips(&pool, &branches);
        let got: Vec<_> = left.iter().map(|l| (l.id.as_str(), l.cause.clone())).collect();
        assert_eq!(
            got,
            [
                ("sp-m", Cause::TipMoved { branch_tip: "new".into() }),
                ("sp-x", Cause::TipMoved { branch_tip: "no branch".into() })
            ]
        );
    }

    #[test_id(t0knoks)]
    #[test]
    fn tip_current_admits_the_live_tip_and_excludes_a_moved_one() {
        let mut branches = BTreeMap::new();
        branches.insert("sp-moved".to_string(), "newtip".to_string());
        branches.insert("sp-stable".to_string(), "sametip".to_string());

        assert!(
            !tip_current("oldtip", "sp-moved", &branches),
            "a certified tip that the branch has moved past must not be admitted"
        );
        // POSITIVE CONTROL: the same branch's own live tip is admitted — proves the check
        // discriminates on the tip, not on the id being present at all.
        assert!(tip_current("newtip", "sp-moved", &branches));
        assert!(tip_current("sametip", "sp-stable", &branches));
        assert!(
            !tip_current("sametip", "sp-unknown", &branches),
            "no branch at all is not current"
        );
    }
    #[test_id(t3wks9c)]
    #[test]
    fn a_member_stacked_on_a_held_bead_is_left_out_of_the_pool_and_returns_when_the_hold_lifts() {
        let member = |id: &str, prereq: Option<&str>| Member {
            id: id.into(),
            tip: format!("{id}-tip"),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: prereq
                .map(|p| (p.to_string(), format!("{p}-tip")))
                .into_iter()
                .collect(),
            blocked_by: vec![],
        provisional: false,
        };
        let pool = || {
            vec![
                member("sp-q", Some("sp-p")),
                member("sp-r", Some("sp-q")),
                member("sp-s", None),
            ]
        };
        let held: std::collections::BTreeSet<String> = ["sp-p".to_string()].into();
        let (kept, dropped) = drop_stacked_on_barred(pool(), held);
        let kept: Vec<String> = kept.into_iter().map(|m| m.id).collect();
        assert_eq!(kept, ["sp-s"]);
        let causes: Vec<(String, Cause)> = dropped.into_iter().map(|l| (l.id, l.cause)).collect();
        assert_eq!(
            causes,
            [
                ("sp-q".to_string(), Cause::StackedOn { prereq: "sp-p".into() }),
                ("sp-r".to_string(), Cause::StackedOn { prereq: "sp-q".into() }),
            ]
        );
        let (lifted, none) = drop_stacked_on_barred(pool(), Default::default());
        assert_eq!(lifted.len(), 3);
        assert!(none.is_empty());
    }
}

#[cfg(test)]
mod result_path_tests {
    use super::*;

    #[test_id(txs9lhr)]
    #[test]
    fn a_rounds_send_back_files_name_the_pass_and_the_attribution_and_two_ejects_share_one_entry() {
        let (ev, pass) = (tmpdir("sb-ev"), tmpdir("sb-pass"));
        fs::write(pass.path().join("spira-lint.out"), "lint red").unwrap();
        fs::write(ev.path().join("test-a.sh.out"), "a red").unwrap();
        fs::create_dir(ev.path().join("key1")).unwrap();
        fs::write(ev.path().join("key1").join("test-b.sh.out"), "b").unwrap();
        let files = send_back_files(ev.path(), Some(pass.path()));
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"pass-spira-lint.out") && names.contains(&"attribution-test-a.sh.out") && names.contains(&"attribution-key1-test-b.sh.out"), "{names:?}");
        assert!(names.iter().all(|n| !n.contains('/')));
        let mut cache = None;
        let mut made = 0;
        let a = send_back_entry(&mut cache, || { made += 1; Ok("round-1-0".into()) });
        let b = send_back_entry(&mut cache, || { made += 1; Ok("round-2-0".into()) });
        assert_eq!((a, b, made), (Some("round-1-0".into()), Some("round-1-0".into()), 1));
    }

    fn tmpdir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("batcher-cut-results-{tag}"))
    }

    #[test_id(tfbxxy0)]
    #[test]
    fn reads_result_one_level_down_under_the_batch_key() {
        let d = tmpdir("nested");
        fs::create_dir_all(d.join("batch-abc")).unwrap();
        fs::write(d.join("batch-abc/test-x.sh.result"), "ok 3s\\n").unwrap();
        assert_eq!(result_status(&d, "test-x.sh"), Some(true));
    }

    #[test_id(tzg4e4x)]
    #[test]
    fn top_level_result_still_read() {
        let d = tmpdir("top");
        fs::write(d.join("test-y.sh.result"), "ok 1s\\n").unwrap();
        assert_eq!(result_status(&d, "test-y.sh"), Some(true));
        fs::write(d.join("test-q.sh.result"), "quarantined-red 1 2 fp p e 1").unwrap();
        assert_eq!(
            result_status(&d, "test-q.sh"),
            Some(true),
            "quarantined never blocks"
        );
        fs::write(d.join("test-t.sh.result"), "timeout 1 2 fp p e 124").unwrap();
        assert_eq!(result_status(&d, "test-t.sh"), Some(false));
        fs::write(d.join("test-h.sh.result"), "").unwrap();
        assert_eq!(
            result_status(&d, "test-h.sh"),
            None,
            "half-written: not final yet"
        );
    }

    #[test_id(tphuunp)]
    #[test]
    fn absent_result_is_not_final_and_never_green() {
        let d = tmpdir("absent");
        assert_eq!(result_status(&d, "test-z.sh"), None);
    }

    /// sp-gjx1b (testenv DESIGN.md §3.7): an undeclared SKIP/SKIP-REQ is reclassified to a
    /// plain `red` status — this reader keys on field 1 only, so it needs no change to block
    /// on it, exactly like any other red. A *declared* skip/skip-req still passes.
    #[test_id(tjwvdm9)]
    #[test]
    fn an_undeclared_skip_reclassified_by_testenv_blocks_like_any_other_red() {
        let d = tmpdir("skip-contract");
        fs::write(
            d.join("test-f.sh.result"),
            "red 1 3 skip:server_testdb_not_available parallel diff 77",
        )
        .unwrap();
        assert_eq!(
            result_status(&d, "test-f.sh"),
            Some(false),
            "an undeclared skip must block like any other red"
        );
        fs::write(
            d.join("test-e.sh.result"),
            "red 1 0 requires:reallymissing parallel diff -",
        )
        .unwrap();
        assert_eq!(result_status(&d, "test-e.sh"), Some(false));
        // a declared skip/skip-req is unaffected: still green
        fs::write(
            d.join("test-g.sh.result"),
            "skip 1 1 skip:widget_missing_(declared) parallel diff 77",
        )
        .unwrap();
        assert_eq!(result_status(&d, "test-g.sh"), Some(true));
    }

    /// sp-3azqi: podman's own exec losing the exit status is a complete, final `.result`
    /// line — never "not final yet" (the `None` a half-written or absent file gets), and
    /// never a pass. This reader must say so on its own, not rely only on the caller's
    /// cross-check against the job's testenv exit code (vm.rs's own belt-and-suspenders).
    #[test_id(thsrx1y)]
    #[test]
    fn a_fault_result_is_final_and_never_green() {
        let d = tmpdir("fault");
        fs::write(
            d.join("test-strand-reclaim-n.sh.result"),
            "fault 1790897469 246 fault:podman-exec-lost parallel explicit 255",
        )
        .unwrap();
        assert_eq!(
            result_status(&d, "test-strand-reclaim-n.sh"),
            Some(false),
            "a fault must read as final and blocking, never as pending or green"
        );
    }
}

#[cfg(test)]
mod open_batch_tests {
    //! The open batch as the batcher reads and opens it: through `queue batch show` and
    //! `queue batch open`, never a file of its own.
    use super::*;

    fn members() -> Vec<(String, String)> {
        vec![
            ("sp-a".into(), "aaaa".into()),
            ("sp-b".into(), "bbbb".into()),
        ]
    }

    #[test_id(tb2urdj)]
    #[test]
    fn the_open_batch_is_what_queue_batch_show_printed_and_none_when_it_printed_nothing() {
        assert!(open_batch_from("").is_none());
        let ob = open_batch_from("batch_id=deck-1\nrepo=deck\nhead=h1\nbase=b0\nmembers=sp-a:aaaa sp-b:bbbb\nbranch=spira/queue/x\nforge.phase=armed\nowner=batcher\npr=12\n").unwrap();
        assert_eq!(ob.pr, "12");
        assert_eq!(ob.members, members());
        let opening = open_batch_from("batch_id=deck-2\nforge.phase=opening\n").unwrap();
        assert_eq!(
            opening.pr, "",
            "a batch still opening is open, with no PR yet"
        );
    }

    #[test_id(t1gaoyx)]
    #[test]
    fn queue_batch_open_answers_its_pr_and_whether_the_open_finished() {
        let run = |code, out: &str| VerbRun {
            code: Some(code),
            out: out.into(),
            err: "queue.sh open-batch: PR 12 for deck is open but auto-merge could not be armed"
                .into(),
        };
        assert_eq!(
            opened_pr(&run(0, "x\nbatch_id=deck-1\npr=12\n")).unwrap(),
            ("12".to_string(), Ok(()))
        );
        let (pr, finished) = opened_pr(&run(1, "batch_id=deck-1\npr=12\n")).unwrap();
        assert_eq!(pr, "12");
        assert!(finished.unwrap_err().contains("queue batch open exited 1"));
        assert!(opened_pr(&run(1, "queue.sh open-batch: no cut\n")).is_err());
    }
}

#[cfg(test)]
mod sifted_state_tests {
    use super::sifted_accepts;

    #[test_id(t6djrau)]
    #[test]
    fn a_pass_is_recorded_only_where_the_machine_accepts_it() {
        assert!(sifted_accepts("SUBMITTED") && sifted_accepts("CERTIFIED"));
        assert!(!sifted_accepts("REWORK") && !sifted_accepts(""));
    }
}

#[cfg(test)]
mod fail_line_tests {
    use super::*;

    #[test_id(tl82fvk)]
    #[test]
    fn a_red_rounds_suites_and_first_fail_lines_are_kept_on_disk_and_in_the_reason() {
        let run = testkit::TempDir::new("batcher-cut-red-evidence");
        let results = run.path().join("rounds").join("b1.results");
        fs::create_dir_all(&results).unwrap();
        fs::write(
            results.join("test-a.sh.out"),
            "ok 1\nFAIL: a broke\nFAIL: later\n",
        )
        .unwrap();
        fs::write(results.join("test-b.sh.out"), "not ok 2 - b broke\n").unwrap();
        let reds = vec!["test-a.sh".to_string(), "test-b.sh".to_string()];
        let why = keep_red_evidence(run.path(), "b1", "judged red: VERDICT", &reds, Ok(7));
        let at = why.lines().last().unwrap().strip_prefix("evidence kept: ").expect(&why);
        assert!(!at.starts_with(results.to_str().unwrap()), "{at}");
        let kept = fs::read_to_string(at).unwrap();
        for text in [&why, &kept] {
            assert!(
                text.contains("judged red: VERDICT")
                    && text.contains("test-a.sh: FAIL: a broke")
                    && text.contains("test-b.sh: not ok 2 - b broke"),
                "{text}"
            );
            assert!(!text.contains("later"), "{text}");
        }
    }

    #[test_id(tpwujbg)]
    #[test]
    fn a_red_unit_phase_with_a_long_running_test_keeps_how_long_it_ran_and_how_it_ended() {
        let run = testkit::TempDir::new("batcher-cut-long-running");
        let results = run.path().join("rounds").join("b1.results");
        fs::create_dir_all(&results).unwrap();
        fs::write(
            results.join("integration.json"),
            r#"{"stage":"integration-tests","verdict":"red","failed":["a::slow"],"long_running":[{"test":"a::slow","over_secs":60,"end":"passed"},{"test":"a::lost","over_secs":120,"end":"unfinished"}]}"#,
        )
        .unwrap();
        let why = keep_red_evidence(run.path(), "b1", "red", &["unit-tests".to_string()], Ok(7));
        assert!(why.contains("a::slow ran over 60 s, then passed") && why.contains("a::lost ran over 120 s, did not finish"), "{why}");
    }

    #[test_id(tbucgnt)]
    #[test]
    fn keeping_a_rounds_results_prunes_only_its_repos_old_closed_rounds() {
        let run = testkit::TempDir::new("batcher-cut-prune-results");
        let rounds = run.path().join("rounds");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400);
        for n in ["spira-old.results", "spira-open.results", "cubby-old.results"] {
            fs::create_dir_all(rounds.join(n)).unwrap();
            fs::File::open(rounds.join(n)).unwrap().set_modified(old).unwrap();
        }
        let corpus = run.path().join("corpus");
        fs::create_dir_all(&corpus).unwrap();
        fs::write(corpus.join("test-red.sh.out"), "FAIL\n").unwrap();
        let prune = RoundsPrune {
            repo: "spira".into(),
            open: vec!["spira-open".into(), "spira-b9".into()],
            retention: spira_config::results_retention::Retention { keep_days: 7, max: 50 },
        };
        let kept = keep_round_results(run.path(), "spira-b9", &corpus, Some(&prune));
        assert!(!rounds.join("spira-old.results").exists());
        assert!(rounds.join("spira-open.results").exists());
        assert!(rounds.join("cubby-old.results").exists());
        assert!(kept.join("test-red.sh.out").exists());
    }

    #[test_id(t3x0c4o)]
    #[test]
    fn a_red_rounds_suite_output_is_copied_under_the_rounds_results_and_named_in_the_reason() {
        let run = testkit::TempDir::new("batcher-cut-keep-results");
        let corpus = run.path().join("batch-results").join("r-1").join("corpus");
        fs::create_dir_all(corpus.join("v1")).unwrap();
        fs::write(corpus.join("test-red.sh.out"), "FAIL: it broke\n").unwrap();
        fs::write(
            corpus.join("v1").join("test-nested.sh.out"),
            "FAIL: nested\n",
        )
        .unwrap();
        let kept = keep_round_results(run.path(), "b9", &corpus, None);
        assert_eq!(kept, run.path().join("rounds").join("b9.results"));
        assert_eq!(
            fs::read_to_string(kept.join("test-red.sh.out")).unwrap(),
            "FAIL: it broke\n"
        );
        let why = keep_red_evidence(
            run.path(),
            "b9",
            "red",
            &["test-red.sh".to_string(), "test-nested.sh".to_string()],
            Ok(7),
        );
        assert!(
            why.contains("test-red.sh: FAIL: it broke")
                && why.contains("test-nested.sh: FAIL: nested"),
            "{why}"
        );
        assert!(why.contains("evidence kept: "), "{why}");
    }

    #[test_id(tlbh2e1)]
    #[test]
    fn kept_red_evidence_names_the_failed_tests_of_a_phase_and_outlives_the_next_passs_wipe() {
        let run = testkit::TempDir::new("batcher-cut-evidence-outlives");
        let results = run.path().join("rounds").join("b1.results");
        fs::create_dir_all(&results).unwrap();
        let test = "the_wal_stays_within_three_checkpoint_intervals_while_reads_never_stop";
        fs::write(results.join("integration-tests.out"), format!("running 1 test\nFAIL {test}\n    thread 'x' panicked at x.rs:1:1:\n    boom\n")).unwrap();
        fs::write(results.join("integration.json"), format!(r#"{{"stage":"integration-tests","verdict":"red","failed":["{test}"]}}"#)).unwrap();
        let why = keep_red_evidence(run.path(), "b1", "red", &["unit-tests".to_string()], Ok(7));
        fs::remove_dir_all(&results).unwrap();
        let at = why.lines().last().unwrap().strip_prefix("evidence kept: ").expect(&why);
        let kept = fs::read_to_string(at).unwrap();
        assert!(kept.contains(&format!("integration-tests failed: {test}")) && kept.contains(&format!("FAIL {test}")), "{kept}");
        assert!(kept.contains("panicked at x.rs:1:1"), "{kept}");
    }

    fn pass_and_corpus(run: &Path, corpus_has_b: bool) -> (PathBuf, PathBuf) {
        let pass = run.join("pass");
        let corpus = run.join("corpus");
        fs::create_dir_all(&pass).unwrap();
        fs::create_dir_all(&corpus).unwrap();
        fs::write(pass.join("test-a.sh.out"), "not ok 1 - broke\n").unwrap();
        fs::write(pass.join("test-a.sh.result"), "red\n").unwrap();
        fs::write(pass.join("test-b.sh.out"), "old").unwrap();
        if corpus_has_b {
            fs::write(corpus.join("test-b.sh.out"), "new").unwrap();
        }
        (pass, corpus)
    }

    #[test_id(tdjbyzs)]
    #[test]
    fn a_rerun_suites_output_is_laid_over_the_passs_after_the_round_tree_is_gone() {
        let run = testkit::TempDir::new("batcher-cut-overlay");
        let (pass, corpus) = pass_and_corpus(run.path(), true);
        keep_round_results(run.path(), "b1", &pass, None);
        fs::remove_dir_all(&pass).unwrap();
        let kept = overlay_round_results(run.path(), "b1", &corpus);
        assert_eq!(fs::read_to_string(kept.join("test-a.sh.out")).unwrap(), "not ok 1 - broke\n");
        assert_eq!(fs::read_to_string(kept.join("test-b.sh.out")).unwrap(), "new");
        assert_eq!(fs::read_to_string(kept.join("test-a.sh.result")).unwrap(), "red\n");
    }

    #[test_id(twayf3s)]
    #[test]
    fn an_attribution_without_a_pass_replaces_the_kept_results_and_one_with_a_pass_keeps_them() {
        let run = testkit::TempDir::new("batcher-cut-attr-end");
        let (pass, corpus) = pass_and_corpus(run.path(), true);
        keep_round_results(run.path(), "b1", &pass, None);
        let kept = keep_attribution_results(run.path(), "b1", &corpus, true, None);
        assert!(kept.join("test-a.sh.out").is_file());
        let kept = keep_attribution_results(run.path(), "b1", &corpus, false, None);
        assert!(!kept.join("test-a.sh.out").exists());
        assert_eq!(fs::read_to_string(kept.join("test-b.sh.out")).unwrap(), "new");
    }

    #[test_id(t92tr1z)]
    #[test]
    fn an_attribution_after_the_kept_pass_itself_leaves_its_results_in_place() {
        let run = testkit::TempDir::new("batcher-cut-attr-kept");
        let (pass, _) = pass_and_corpus(run.path(), true);
        let kept = keep_round_results(run.path(), "b1", &pass, None);
        keep_attributed_pass(run.path(), "b1", &kept, None);
        assert_eq!(fs::read_to_string(kept.join("test-a.sh.out")).unwrap(), "not ok 1 - broke\n");
        assert_eq!(fs::read_to_string(kept.join("test-a.sh.result")).unwrap(), "red\n");
    }

    #[test_id(t0jjrkx)]
    #[test]
    fn an_attribution_after_the_kept_pass_under_another_spelling_leaves_its_results_in_place() {
        let run = testkit::TempDir::new("batcher-cut-attr-kept-spelling");
        let (pass, _) = pass_and_corpus(run.path(), true);
        let kept = keep_round_results(run.path(), "b1", &pass, None);
        let other = kept.join("..").join(kept.file_name().unwrap());
        keep_attributed_pass(run.path(), "b1", &other, None);
        assert_eq!(fs::read_to_string(kept.join("test-a.sh.result")).unwrap(), "red\n");
    }

    #[test_id(tnilu09)]
    #[test]
    fn an_attribution_that_ran_nothing_leaves_the_passs_output_kept_and_cited() {
        let run = testkit::TempDir::new("batcher-cut-overlay-empty");
        let (pass, corpus) = pass_and_corpus(run.path(), false);
        keep_round_results(run.path(), "b1", &pass, None);
        fs::remove_dir_all(&pass).unwrap();
        let kept = overlay_round_results(run.path(), "b1", &corpus);
        assert_eq!(fs::read_to_string(kept.join("test-a.sh.out")).unwrap(), "not ok 1 - broke\n");
        let why = keep_red_evidence(run.path(), "b1", "red", &["test-a.sh".to_string()], Ok(7));
        assert!(why.contains("test-a.sh: not ok 1 - broke"), "{why}");
    }

    #[test_id(tw13cks)]
    #[test]
    fn an_ejected_members_digest_is_built_from_its_red_suites_output_alone() {
        let d = testkit::TempDir::new("batcher-cut-ejection");
        fs::write(d.path().join("test-a.sh.out"), "---- t::boom stdout ----\nthread 't::boom' panicked at x/y.rs:7:1:\nit broke\n\nfailures:\n").unwrap();
        fs::write(d.path().join("test-green.sh.out"), "all fine\n").unwrap();
        let steps = red_suite_steps(d.path(), None, &["test-a.sh".to_string(), "test-missing.sh".to_string()]);
        let digest = spira_config::ejection::digest_steps("t1", &[], &[], &steps);
        assert!(digest.contains("FAIL t::boom at x/y.rs:7") && digest.contains("it broke") && !digest.contains("all fine"), "{digest}");
    }

    #[test_id(thmsrbv)]
    #[test]
    fn an_attributions_ejection_reads_the_red_from_the_pass_results() {
        let results = testkit::TempDir::new("batcher-cut-results");
        let pass = testkit::TempDir::new("batcher-cut-pass");
        fs::write(pass.path().join("spira-lint.out"), "pass lint text\n").unwrap();
        fs::write(pass.path().join("both.out"), "from pass\n").unwrap();
        fs::write(results.path().join("both.out"), "from results\n").unwrap();
        let suites = ["spira-lint".to_string(), "both".to_string()];
        let steps = red_suite_steps(results.path(), Some(pass.path()), &suites);
        assert_eq!(steps, vec![("spira-lint".to_string(), "pass lint text\n".to_string()), ("both".to_string(), "from results\n".to_string())]);
        assert!(red_suite_steps(results.path(), None, &suites[..1]).is_empty());
    }

    #[test_id(te988qj)]
    #[test]
    fn a_sole_members_record_has_a_step_per_red_suite_only_when_the_suites_are_named_separately() {
        let results = testkit::TempDir::new("batcher-cut-sole-results");
        let pass = testkit::TempDir::new("batcher-cut-sole-pass");
        fs::write(pass.path().join("test-a.sh.out"), "ok 1 - fine\nnot ok 2 - it broke\n").unwrap();
        fs::write(pass.path().join("test-b.sh.out"), "not ok 1 - also broke\n").unwrap();
        let digest = |suites: &[String]| {
            spira_config::ejection::digest_steps("t", &[], &[], &red_suite_steps(results.path(), Some(pass.path()), suites))
        };
        let apart = digest(&["test-a.sh".to_string(), "test-b.sh".to_string()]);
        for want in ["STEP test-a.sh", "not ok 2 - it broke", "STEP test-b.sh", "not ok 1 - also broke"] {
            assert!(apart.contains(want), "{want}: {apart}");
        }
        let joined = digest(&["test-a.sh,test-b.sh".to_string()]);
        assert!(!joined.contains("not ok"), "{joined}");
    }

    #[test_id(tljv5r6)]
    #[test]
    fn each_open_round_carries_its_own_phase() {
        let out = "round=open\nphase=opened\nbatch_id=b1\nround2=open\nround2_phase=certifying\nround2_batch_id=b2\n";
        let phases: Vec<_> = open_rounds_of(out)
            .into_iter()
            .map(|r| (r.id, r.phase))
            .collect();
        assert_eq!(
            phases,
            [
                ("b1".to_string(), "opened".to_string()),
                ("b2".to_string(), "certifying".to_string())
            ]
        );
    }

    #[test_id(tcji1ce)]
    #[test]
    fn the_first_fail_line_is_found_and_absence_is_none() {
        assert_eq!(
            first_fail_line("ok 1\n  FAIL widget broke\nFAIL later\n").as_deref(),
            Some("FAIL widget broke")
        );
        assert_eq!(
            first_fail_line("not ok 3 - x\n").as_deref(),
            Some("not ok 3 - x")
        );
        assert_eq!(first_fail_line("all fine\nPASS\n"), None);
        let signal = "UNIT-TESTS-RED: SIGNAL binary=crashy signal: 11, SIGSEGV\nUNIT-TESTS-CAUSE: exit status 101\n";
        assert_eq!(
            first_fail_line(signal).as_deref(),
            Some("UNIT-TESTS-RED: SIGNAL binary=crashy signal: 11, SIGSEGV"),
            "a classified unit red is its own evidence"
        );
        assert!(
            first_fail_line("UNIT-TESTS-RED: TIMEOUT binary=slow\n").is_some()
                && first_fail_line("UNIT-TESTS-RED: BUILD binary=w\n").is_some()
        );
        let d = testkit::TempDir::new("batcher-cut-failline");
        fs::write(d.join("test-a.sh.out"), "setup\nFAIL: boom\n").unwrap();
        assert_eq!(
            suite_first_fail(&d, None, "test-a.sh").as_deref(),
            Some("FAIL: boom")
        );
        assert_eq!(suite_first_fail(&d, None, "test-missing.sh"), None);
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod land_exit_tests {
    use super::*;

    #[test_id(tfor5k8)]
    #[test]
    fn a_deploy_fault_is_not_a_refusal() {
        assert_eq!(classify_land_exit(Some(0)), LandOutcome::Landed);
        assert_eq!(classify_land_exit(Some(1)), LandOutcome::Refused);
        assert_eq!(classify_land_exit(Some(2)), LandOutcome::Refused);
        assert_eq!(classify_land_exit(Some(3)), LandOutcome::DeployFault);
        assert_eq!(
            classify_land_exit(None),
            LandOutcome::Refused,
            "killed by a signal: nothing is known to have landed"
        );
    }

    /// sp-tjrjca: a base-moved refusal is told by its exit, never by its words.
    #[test_id(tdltj8l)]
    #[test]
    fn a_base_moved_land_refusal_is_read_from_its_exit() {
        assert!(land_base_moved(Some(LAND_BASE_MOVED_EXIT)));
        assert!(!land_base_moved(Some(1)) && !land_base_moved(Some(7)) && !land_base_moved(None));
    }

    /// sp-822b8l: the typed records of a refused open, one member each; other lines are not.
    #[test_id(ti4ubny)]
    #[test]
    fn a_refused_open_names_each_member_by_its_typed_record() {
        let out = "refused=sp-1mspuc kind=integration why=sp-1mspuc: carried by integration sp-ggdrzb — lands with it, not admitted\nnoise\nrefused=sp-zr6wdz kind=round-conflict why=sp-zr6wdz: conflicts with the round — paths: a.rs\n";
        let r = parse_refused(out);
        assert_eq!(
            r.iter()
                .map(|r| (r.id.as_str(), r.kind.as_str()))
                .collect::<Vec<_>>(),
            [
                ("sp-1mspuc", "integration"),
                ("sp-zr6wdz", "round-conflict")
            ]
        );
        assert!(r[1].why.ends_with("paths: a.rs"), "{r:?}");
        assert!(parse_refused("batch=x\nmembers=sp-a:t\n").is_empty());
    }

    fn repo_at(d: &Path) -> Repo {
        Repo {
            name: "spira".into(),
            path: d.to_path_buf(),
            base: "local/main".into(),
            forge: d.to_path_buf(),
            land: Land::Local,
        }
    }

    #[test_id(tg3tlix)]
    #[test]
    fn last_touching_names_the_newest_commit_on_the_paths_and_nothing_for_none() {
        let t = testkit::TempDir::new("batcher-cut-last-touching");
        let d = t.path();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(d)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        git(&["init", "-q"]);
        fs::create_dir_all(d.join("a/tests")).unwrap();
        fs::write(d.join("a/lib.rs"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "touch a"]);
        fs::write(d.join("test-x.sh"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "touch x"]);
        let repo = repo_at(d);
        let got = last_touching(&repo, "HEAD", &["a".to_string()]).unwrap();
        assert!(got.ends_with(" touch a"), "{got}");
        let got = last_touching(&repo, "HEAD", &[":(glob)**/test-x.sh".to_string()]).unwrap();
        assert!(got.ends_with(" touch x"), "{got}");
        assert_eq!(last_touching(&repo, "HEAD", &["nope".to_string()]), None);
        assert_eq!(last_touching(&repo, "HEAD", &[]), None);
    }

    fn claims_repo(t: &testkit::TempDir, base: &str, tip: &[(&str, &str)]) -> (Repo, String, String) {
        let d = t.path();
        let git = |args: &[&str]| testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t"].iter().chain(args).copied().collect::<Vec<_>>());
        git(&["init", "-q", "-b", "main"]);
        fs::write(d.join("a.rs"), base).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        let start = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "-b", "member"]);
        git(&["rm", "-q", "a.rs"]);
        for (path, text) in tip {
            fs::create_dir_all(d.join(path).parent().unwrap()).unwrap();
            fs::write(d.join(path), text).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "member"]);
        let tip = git(&["rev-parse", "HEAD"]);
        (repo_at(d), start, tip)
    }

    const CLAIM_BASE: &str = "#[test_id(t0aaaaa)]\n#[test]\nfn t() {\n    one();\n}\n#[test_id(t0bbbbb)]\n#[test]\nfn u() {\n    two();\n}\n";

    #[test_id(tcl1a2b)]
    #[test]
    fn a_member_that_edits_inside_a_disabled_tests_body_claims_its_row_and_one_that_edits_another_test_does_not() {
        let rows = vec!["test:t0aaaaa".to_string()];
        let t = testkit::TempDir::new("batcher-cut-claim-edit");
        let (repo, base, tip) = claims_repo(&t, CLAIM_BASE, &[("a.rs", &CLAIM_BASE.replace("one()", "three()"))]);
        assert_eq!(member_claims(&repo, &base, &tip, &rows), ["test:t0aaaaa"]);
        let t = testkit::TempDir::new("batcher-cut-claim-other");
        let (repo, base, tip) = claims_repo(&t, CLAIM_BASE, &[("a.rs", &CLAIM_BASE.replace("two()", "three()"))]);
        assert_eq!(member_claims(&repo, &base, &tip, &rows), Vec::<String>::new());
    }

    #[test_id(tcl3c4d)]
    #[test]
    fn a_member_that_renames_and_moves_the_test_with_an_identical_body_claims_nothing() {
        let t = testkit::TempDir::new("batcher-cut-claim-move");
        let moved = CLAIM_BASE.replace("fn t()", "fn renamed()");
        let (repo, base, tip) = claims_repo(&t, CLAIM_BASE, &[("sub/b.rs", &moved)]);
        assert_eq!(member_claims(&repo, &base, &tip, &["test:t0aaaaa".to_string()]), Vec::<String>::new());
    }

    #[test_id(tcl5e6f)]
    #[test]
    fn a_landing_with_a_claimed_row_standing_activates_it_once_naming_the_member_and_round() {
        let row = |claimed_by: Option<&str>| crate::vm::LiftedRow { state: "disabled".into(), test: "test:t0aaaaa".into(), bead: "sp-own".into(), until: -1, claimed_by: claimed_by.map(String::from) };
        let rows = LiftedRows::new(vec![row(Some("sp-m")), crate::vm::LiftedRow { test: "test:t0bbbbb".into(), ..row(None) }]);
        let mut seen = vec![];
        let failed = activate_claimed_with(
            &mut |t, r| {
                seen.push((t.to_string(), r.to_string()));
                Ok(())
            },
            "r9",
            &rows.standing_claims(),
        );
        assert!(failed.is_empty());
        assert_eq!(seen, [("test:t0aaaaa".to_string(), "body changed by sp-m; green in round r9".to_string())]);
        assert_eq!(rows.standing()[0], ("test:t0aaaaa".to_string(), "sp-m".to_string()), "the claimer owns the row for the round");
    }

    #[test_id(tb7u376)]
    #[test]
    fn a_red_ci_implicates_only_the_member_whose_changed_file_it_names() {
        let changed = vec![
            ("sp-a".to_string(), vec!["crates/x/src/a.rs".to_string()]),
            (
                "sp-b".to_string(),
                vec!["crates/x/src/parent.rs".to_string()],
            ),
        ];
        let out = "Diff in crates/x/src/parent.rs:30:\n";
        assert_eq!(implicated(out, &changed), vec!["sp-b".to_string()]);
        assert!(implicated("boom", &changed).is_empty());
        assert_eq!(implicated("boom", &changed[..1]), vec!["sp-a".to_string()]);
    }

    #[test_id(tju3g39)]
    #[test]
    fn a_filed_incident_is_the_id_in_the_typed_answer_not_the_last_line() {
        assert_eq!(filed_id("{\"id\":\"sp-abc\"}\n"), Ok("sp-abc".to_string()));
        assert!(
            filed_id("sp-abc\n").is_err(),
            "a bare line is not an answer"
        );
        assert!(filed_id("filed sp-abc\nnoted\n").is_err());
        assert!(filed_id("{\"id\":null}").is_err(), "a skew filed nothing");
        assert!(filed_id("").is_err());
    }

    #[test_id(tawaqbu)]
    #[test]
    fn a_promotion_names_its_round_head_members_and_whether_it_was_attested() {
        let p = parse_promotion("pass ran\n{\"batch\":\"r2\",\"head\":\"abc\",\"members\":[{\"id\":\"sp-a\",\"tip\":\"t1\"},{\"id\":\"sp-b\",\"tip\":\"t2\"}],\"attested\":true}\n").unwrap();
        assert_eq!(
            p,
            Promotion {
                batch: "r2".into(),
                head: "abc".into(),
                members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
                attested: true
            }
        );
        assert!(!parse_promotion("{\"batch\":\"r2\",\"head\":\"abc\",\"members\":[{\"id\":\"sp-a\",\"tip\":\"t1\"}],\"attested\":false}\n").unwrap().attested);
        assert!(
            parse_promotion("batch=r2\nhead=abc\nmembers=sp-a:t1\nattested=1\n").is_none(),
            "key=value lines are no answer"
        );
        assert_eq!(
            parse_promotion("batch=r2\nmembers=sp-a:t1\n"),
            None,
            "a promotion with no head names no tree to land"
        );
    }

    #[test_id(ttcppsv)]
    #[test]
    fn a_landing_clears_a_loop_pause_but_never_a_deliberate_hold() {
        let d = testkit::TempDir::new("batcher-cut-unpause");
        let pause = d.join("cut.pause");
        let mut e = lifecycle_tests_env(&d);
        assert_eq!(unpause_after_land(&e).unwrap(), Unpause::Off, "default off");
        fs::write(&pause, "ROUND RED").unwrap();
        assert!(
            pause.exists(),
            "positive control: the pause exists before the call"
        );
        assert_eq!(unpause_after_land(&e).unwrap(), Unpause::Off);
        assert!(pause.exists(), "disabled leaves the pause alone");
        e.land_unpause = Some(pause.clone());
        assert_eq!(unpause_after_land(&e).unwrap(), Unpause::Cleared);
        assert!(!pause.exists());
        assert_eq!(unpause_after_land(&e).unwrap(), Unpause::Absent);
        fs::write(&pause, "hold: publish red").unwrap();
        assert_eq!(unpause_after_land(&e).unwrap(), Unpause::Held);
        assert!(pause.exists(), "a deliberate hold survives a landing");
    }

    #[test_id(th805e4)]
    #[test]
    fn rounds_stacked_on_one_unlanded_head_share_one_incident_key() {
        let d = testkit::TempDir::new("batcher-cut-stacked-key");
        let repo = repo_at(&d);
        let refusal = |parent: &str| {
            format!("refused: queue.sh round land: round r3 is stacked on round {parent}, which has not landed — land {parent} first; nothing changed")
        };
        let keys: Vec<_> = [("h1", "r1"), ("h2", "r1"), ("h3", "r2")]
            .iter()
            .map(|(h, p)| land_unverified_key(&repo, h, &refusal(p)))
            .collect();
        assert_eq!(keys[0], keys[1], "{keys:?}");
        assert_ne!(
            keys[0], keys[2],
            "a second unlanded head is a second incident: {keys:?}"
        );
        assert_ne!(
            land_unverified_key(&repo, "h1", "refused: not a fast-forward"),
            land_unverified_key(&repo, "h2", "refused: not a fast-forward"),
            "positive control: other refusals stay one incident per head"
        );
    }

    #[test_id(tyl34h2)]
    #[test]
    fn a_round_of_the_stores_view_is_listed_as_round_status_lists_it() {
        let record = store_view::Record {
            id: "r2".into(),
            state: "CI_RUNNING".into(),
            fields: [("head", "bbb"), ("members", "sp-c:tc sp-d:td"), ("worktree", "w2"), ("opened", "1000"), ("red", "test-x.sh"), ("ejected", "sp-a sp-b "), ("ejected_by", "batcher concierge ")].map(|(k, v)| (k.to_string(), v.to_string())).to_vec(),
        };
        let round = store_view::Round { phase: "certifying".into(), record };
        assert_eq!(
            open_round_of(&round, 1090),
            OpenRound { id: "r2".into(), members: vec!["sp-c".into(), "sp-d".into()], tips: vec!["tc".into(), "td".into()], head: "bbb".into(), worktree: "w2".into(), phase: "certifying".into(), red: true, red_suites: vec!["test-x.sh".into()], ejected: vec!["sp-a".into(), "sp-b".into()], ejected_by: vec!["batcher".into(), "concierge".into()], age_secs: 90 }
        );
    }

    #[test_id(tu9esdp)]
    #[test]
    fn status_lists_every_open_round_with_its_head_and_age() {
        let out = "rounds=2\nround=open\nbatch_id=r1\nhead=h1\nmembers=sp-a:ta sp-b:tb\nworktree=/w/1\nwall_secs=90\nround2=open\nround2_batch_id=r2\nround2_head=h2\nround2_members=sp-c:tc\nround2_worktree=/w/2\nround2_red=test-x.sh\nround2_wall_secs=30\nstaged_members=sp-z\n";
        let rounds = open_rounds_of(out);
        assert_eq!(
            rounds
                .iter()
                .map(|r| (r.id.as_str(), r.head.as_str(), r.red, r.age_secs))
                .collect::<Vec<_>>(),
            [("r1", "h1", false, 90), ("r2", "h2", true, 30)]
        );
        assert_eq!(rounds[0].members, ["sp-a", "sp-b"]);
        assert_eq!(rounds[0].tips, ["ta", "tb"]);
        assert_eq!(split_members("sp-a:ta sp-b"), (vec!["sp-a".to_string(), "sp-b".to_string()], vec!["ta".to_string(), String::new()]));
        assert!(open_rounds_of("round=none\nrepo=spira\n").is_empty());
    }

    #[test_id(tj64txc)]
    #[test]
    fn status_lists_the_beads_a_round_has_had_ejected() {
        let out = "round=open\nbatch_id=r1\nejected=sp-a sp-b \nround2=open\nround2_batch_id=r2\nround2_ejected=sp-c \n";
        let rounds = open_rounds_of(out);
        assert_eq!((rounds[0].ejected.clone(), rounds[1].ejected.clone()), (vec!["sp-a".to_string(), "sp-b".to_string()], vec!["sp-c".to_string()]));
    }

    #[test_id(tp6mqdm)]
    #[test]
    fn status_lists_who_ejected_each_ejected_bead_in_order() {
        let out = "round=open\nbatch_id=r1\nejected=sp-a sp-b \nejected_by=batcher concierge \n";
        assert_eq!(open_rounds_of(out)[0].ejected_by, ["batcher", "concierge"]);
    }
}

#[cfg(test)]
mod wait_lock_tests {
    use super::*;

    #[test_id(tf2kdhd)]
    #[test]
    fn a_held_queue_lock_beats_until_it_drops() {
        let d = testkit::TempDir::new("batcher-cut-beat");
        let e = super::lifecycle_tests_env(&d);
        let beat = e.queue_dir.join("r").join(BEAT_FILE);
        let held = try_lock(&e, "r").unwrap().unwrap();
        assert!(
            beat.is_file(),
            "the batcher's queue-lock holder must beat while it lands"
        );
        drop(held);
        assert!(!beat.exists(), "the beat goes with the lock");
    }

    #[test_id(tb57yj6)]
    #[test]
    fn a_leg_keeps_its_result_when_another_operation_holds_the_lock_past_the_wait() {
        let d = testkit::TempDir::new("batcher-cut-retake");
        let e = super::lifecycle_tests_env(&d);
        let _parked = park_queue_lock(try_lock(&e, "r").unwrap().unwrap());
        let (e2, mut other) = (e.clone(), None);
        let out = unlocked(&e, "r", Some("b1"), 1, || {
            other = try_lock(&e2, "r").unwrap();
            let held = other.take();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(2500)); // clock-safe: holds the lock past the 1s wait, then releases it
                drop(held);
            });
            "verdict"
        });
        assert_eq!(
            out,
            Ok("verdict"),
            "a lock held past the wait must not cost the leg its result"
        );
        assert!(
            try_lock(&e, "r").unwrap().is_none(),
            "the queue lock is held again after the retry"
        );
    }

    #[test_id(tuot37n)]
    #[test]
    fn touch_refreshes_a_stale_beat_file() {
        let d = testkit::TempDir::new("batcher-cut-beat2");
        let p = d.path().join("b");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        fs::File::create(&p).unwrap().set_modified(old).unwrap();
        touch(&p);
        // clock-safe: compares file mtimes against a stamp this test set; no duration is measured
        assert!(fs::metadata(&p).unwrap().modified().unwrap() > old);
        let b = Beat::start(p.clone(), std::time::Duration::from_secs(3600));
        assert!(p.is_file());
        drop(b);
        assert!(!p.exists());
    }
}

/// A scratch Env for unit tests, this crate's and its binary's: nothing points anywhere real.
#[doc(hidden)]
pub fn lifecycle_tests_env(dir: &Path) -> Env {
    Env {
        home: dir.to_path_buf(),
        run: dir.to_path_buf(),
        queue_dir: dir.join("queue"),
        forge: dir.join("forge"),
        tsd_bin: None,
        round_vm: dir.join("round-vm"),
        queue_bin: dir.join("queue"),
        rebase_stale_bin: dir.join("rebase-stale"),
        testenv_bin: dir.join("testenv"),
        gate_bin: dir.join("gate"),
        test_registry_bin: Some(dir.join("test-registry")),
        round_slots: None,
        hypervisor: Hypervisor {
            host_mib: 0,
            round_vm_mib: 0,
            runner_mib: 0,
        },
        poll: std::time::Duration::from_secs(1),
        maxpar: 1,
        wall_secs: 1,
        stall_secs: 1,
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_rows: Default::default(),
        lc_bin: None,
        lc_timeout: 5,
        verdicts: dir.join("verdicts"),
        land_lock_attempts: 3,
        land_lock_wait: Duration::from_millis(1),
        orphan_redrive_after: 600,
        pause_lift_after: 900,
        land_unpause: None,
        results_retention: spira_config::results_retention::Retention { keep_days: 7, max: 50 },
        commit: Err("no release".into()),
        repo: Err("no release".into()),
    }
}

#[cfg(test)]
mod hypervisor_tests {
    use super::Hypervisor;

    fn hv(host_mib: u64, round_vm_mib: u64) -> Hypervisor {
        Hypervisor {
            host_mib,
            round_vm_mib,
            runner_mib: 16384,
        }
    }

    #[test_id(tt9ur8e)]
    #[test]
    fn two_round_vms_and_a_runner_must_fit() {
        assert!(hv(2 * 65536 + 16384, 65536).fits_a_staged_round());
        assert!(!hv(2 * 65536 + 16383, 65536).fits_a_staged_round());
    }

    #[test_id(t3x416o)]
    #[test]
    fn an_unknown_size_never_fits() {
        assert!(!hv(0, 65536).fits_a_staged_round());
        assert!(!hv(1 << 30, 0).fits_a_staged_round());
    }
}

#[cfg(test)]
mod shots_tests {
    use super::*;

    #[test_id(tgxn6df)]
    #[test]
    fn a_members_pngs_are_linked_and_a_member_without_any_adds_nothing() {
        let tmp = testkit::TempDir::new("pr-shots");
        let d = tmp.path().join("shots/sp-a/abc");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("index-390.png"), b"x").unwrap();
        fs::write(d.join("note.txt"), b"x").unwrap();
        let members = vec![
            ("sp-a".to_string(), "abc".to_string()),
            ("sp-b".to_string(), "def".to_string()),
        ];
        let with = shots_section(tmp.path(), Some("host:1"), &members);
        assert!(
            with.contains("sp-a index-390.png: http://host:1/shots/sp-a/abc/index-390.png")
                && !with.contains("note.txt")
                && !with.contains("sp-b"),
            "{with}"
        );
        assert!(shots_section(tmp.path(), None, &members)
            .contains("- sp-a index-390.png: /shots/sp-a/abc/index-390.png"));
        assert_eq!(shots_section(tmp.path(), None, &members[1..]), "");
    }
}

#[cfg(test)]
mod round_status_tests {
    use super::members_of_batch;

    const STATUS: &str = "rounds=2\nopen_batches=old new\nround=open\nbatch_id=old\nmembers=a:1 b:2\nround2=open\nround2_batch_id=new\nround2_members=c:3\n";

    #[test_id(tk1lv9r)]
    #[test]
    fn the_members_are_those_of_the_named_round_not_the_first_open_one() {
        assert_eq!(members_of_batch(STATUS, "new"), Some(vec!["c".to_string()]));
        assert_eq!(
            members_of_batch(STATUS, "old"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(members_of_batch(STATUS, "gone"), None);
    }
}

#[cfg(test)]
mod forge_release_tests {
    use super::*;

    #[test_id(tmh5ivt)]
    #[test]
    fn a_declared_forge_script_is_a_stand_in_run_with_bash_and_the_default_runs_in_process() {
        let dir = testkit::TempDir::new("batcher-forge-stand-in");
        let script = dir.path().join("forge-fixture.sh");
        std::fs::write(&script, "echo \"$1 $2 $3\"\nexit 0\n").unwrap();
        let repo = Repo {
            name: "r".into(),
            path: PathBuf::from("tmp/r"),
            base: "local/main".into(),
            forge: script,
            land: Land::Forge,
        };
        let r = forge_run(&repo, "pr-arm-queue", &["merge"]).unwrap();
        assert_eq!((r.code, r.out.as_str()), (0, "pr-arm-queue tmp/r merge\n"));
        assert_eq!(
            forge::client::route_of(Path::new("forge")),
            forge::client::Route::InProcess
        );
    }

    #[test_id(tmj5xp0)]
    #[test]
    fn arming_reports_the_forges_lines_and_its_failure() {
        let r = |code| forge::client::Reply {
            code,
            out: "armed #12\n".into(),
            err: String::new(),
        };
        assert_eq!(armed("r", r(0)), Ok(()));
        assert_eq!(armed("r", r(1)), Err("forge pr-arm-queue exited 1".into()));
    }

    #[test_id(toc4tzu)]
    #[test]
    fn a_lifecycle_answer_reads_as_the_childs_did() {
        use spira_store::lc_call::{LcError, Reply};
        assert_eq!(
            lc_answer(Ok(Reply {
                code: 0,
                out: "[]".into(),
                err: String::new(), read_ended: false
            })),
            Ok("[]".into())
        );
        assert_eq!(
            lc_answer(Ok(Reply {
                code: 3,
                out: "x\nrefused: held\n".into(),
                err: String::new(), read_ended: false
            })),
            Err("spira-lc exited 3: refused: held".into())
        );
        assert!(lc_answer(Err(LcError::Fault("cannot tell: down".into())))
            .unwrap_err()
            .contains("cannot tell"));
    }

    #[test_id(td9icok)]
    #[test]
    fn an_open_batch_the_store_cannot_vouch_for_could_not_say() {
        use spira_store::lc_call::LcError;
        use store_view::{BatchView, Record};
        assert!(open_batch_of(Err(LcError::Refused("stale".into())))
            .unwrap_err()
            .starts_with("could not say"));
        assert!(open_batch_of(Ok(BatchView {
            repo: "r".into(),
            open: None
        }))
        .unwrap()
        .is_none());
        let rec = Record {
            id: "b".into(),
            state: "OPEN".into(),
            fields: vec![
                ("batch_id".into(), "b".into()),
                ("members".into(), "sp-a:t1".into()),
                ("pr".into(), "12".into()),
            ],
        };
        let ob = open_batch_of(Ok(BatchView {
            repo: "r".into(),
            open: Some(rec),
        }))
        .unwrap()
        .unwrap();
        assert_eq!(
            (ob.pr.as_str(), ob.members),
            ("12", vec![("sp-a".to_string(), "t1".to_string())])
        );
    }

    #[test_id(ttdfsmt)]
    #[test]
    fn a_certify_failure_is_typed_by_its_exit_code_and_stale_image_never_by_the_code() {
        let kind = |code, err: &str| {
            certify_failure(code, err, "queue round certify exited x".into()).kind
        };
        assert_eq!(kind(Some(CERTIFY_ENV_RC), "no VM"), CutKind::Infra);
        assert_eq!(
            kind(Some(CERTIFY_ENV_RC), "round AMI is stale: clone refused"),
            CutKind::StaleImage
        );
        assert_eq!(
            kind(Some(CERTIFY_LOST_RC), "INCOMPLETE reason=vm-lost"),
            CutKind::Infra
        );
        assert_eq!(
            kind(Some(3), "certify exited 4 in words only"),
            CutKind::Other
        );
        assert_eq!(kind(None, ""), CutKind::Other);
    }
}

#[cfg(test)]
mod ci_red_suites_tests {
    use super::ci_red_suites;

    fn tree(toml: Option<&str>) -> testkit::TempDir {
        let d = testkit::TempDir::new("batcher-ci-red");
        if let Some(t) = toml {
            std::fs::create_dir_all(d.path().join(".spira")).unwrap();
            std::fs::write(d.path().join(".spira/testenv.toml"), t).unwrap();
        }
        d
    }

    #[test_id(touifsv)]
    #[test]
    fn a_ci_red_names_the_suites_the_repos_testenv_declares_not_a_literal_ci() {
        let d = tree(Some("[tier.gate]\ncommand = \"true\"\n"));
        assert_eq!(ci_red_suites(d.path()), vec!["gate".to_string()]);
    }

    #[test_id(tprhfzm)]
    #[test]
    fn a_ci_red_on_a_tree_declaring_no_suite_names_the_whole_testenv_run() {
        assert_eq!(
            ci_red_suites(tree(Some("ci_entry = \"make\"\n")).path()),
            vec!["testenv".to_string()]
        );
        assert_eq!(
            ci_red_suites(tree(None).path()),
            vec!["testenv".to_string()]
        );
    }
}

#[cfg(test)]
mod adds_path_tests {
    use super::adds_path;

    #[test_id(t88iwrx)]
    #[test]
    fn a_tip_adds_only_the_file_it_creates_not_one_it_edits() {
        let root = testkit::TempDir::new("adds-path");
        let d = root.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join("spira")).unwrap();
        std::fs::write(d.join("spira/test-old.sh"), "a\n").unwrap();
        testkit::git(d, &["add", "."]);
        testkit::git(
            d,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "base",
            ],
        );
        let base = testkit::git(d, &["rev-parse", "HEAD"]);
        std::fs::write(d.join("spira/test-new.sh"), "b\n").unwrap();
        std::fs::write(d.join("spira/test-old.sh"), "c\n").unwrap();
        testkit::git(d, &["add", "."]);
        testkit::git(
            d,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "tip",
            ],
        );
        let tip = testkit::git(d, &["rev-parse", "HEAD"]);
        assert!(adds_path(d, base.trim(), tip.trim(), "spira/test-new.sh"));
        assert!(!adds_path(d, base.trim(), tip.trim(), "spira/test-old.sh"));
        assert!(!adds_path(
            d,
            base.trim(),
            "no-such-ref",
            "spira/test-new.sh"
        ));
    }
}

#[cfg(test)]
mod abandon_hold_tests {
    use super::*;
    /// A `queue` whose `round abandon` refuses while `refuse` exists, and whose `round status`
    /// lists `r1` as open until an abandon has gone through.
    fn queue_stub(d: &Path) -> Env {
        let script = format!(
            "#!/bin/sh\ncase \"$2\" in\n abandon) if [ -e {d}/refuse ]; then echo 'queue.sh round abandon: refused' >&2; exit 1; fi; rm -f {d}/live; exit 0;;\n status) if [ -e {d}/live ]; then echo 'round=open'; echo 'batch_id=r1'; echo 'phase=opened'; else echo 'round=none'; fi;;\nesac\n",
            d = d.display()
        );
        testkit::write_exe(d.join("queue"), &script);
        fs::write(d.join("live"), "").unwrap();
        fs::write(d.join("refuse"), "").unwrap();
        lifecycle_tests_env(d)
    }

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.into(),
            path: PathBuf::new(),
            base: "main".into(),
            forge: PathBuf::new(),
            land: Land::Local,
        }
    }

    #[test_id(t50nz1o)]
    #[test]
    fn an_abandon_after_zero_ejects_does_not_claim_an_ejection() {
        let unknown = abandon_considered("red", None);
        assert!(!unknown.contains("after ejecting"), "{unknown}");
        let none = abandon_considered("red", Some(0));
        assert!(!none.contains("after ejecting"), "{none}");
        assert!(none.contains("no member was ejected"), "{none}");
        let some = abandon_considered("ci-red", Some(2));
        assert!(some.contains("after ejecting 2 member(s)"), "{some}");
    }

    #[test_id(tse5clv)]
    #[test]
    fn a_refused_abandon_holds_the_next_cut_until_the_round_is_abandoned() {
        let d = testkit::TempDir::new("abandon-hold");
        let e = queue_stub(d.path());
        let r = repo("alpha");
        assert_eq!(abandon_hold(&e, &r), None, "control: nothing refused, nothing held");
        assert!(!round_abandon(&e, &r, "r1", "red", "ci-red"));
        assert_eq!(abandon_hold(&e, &r).as_deref(), Some("r1"), "still refused: the cut holds");
        fs::remove_file(d.path().join("refuse")).unwrap();
        assert_eq!(abandon_hold(&e, &r), None, "the retry abandoned it: the cut proceeds");
        assert!(
            crate::pace::Files::new(&e.run, "alpha").abandon_refused().is_none(),
            "the record is gone once the round is"
        );
    }

    #[test_id(t3xy3as)]
    #[test]
    fn an_unconverged_abandon_carries_its_cause_and_the_probe_summary_as_considered() {
        let d = testkit::TempDir::new("abandon-considered");
        let script = format!(
            "#!/bin/sh\necho \"$@\" > {d}/argv\necho \"$QUEUE_ABANDON_CONSIDERED\" > {d}/considered\nexit 0\n",
            d = d.path().display()
        );
        testkit::write_exe(d.path().join("queue"), &script);
        let e = lifecycle_tests_env(d.path());
        assert!(round_abandon_considered(&e, &repo("alpha"), "r1", "red", "attribution-not-converging", "8 probes named no member"));
        let argv = fs::read_to_string(d.path().join("argv")).unwrap();
        assert!(argv.contains("abandon r1 alpha") && argv.contains("--cause attribution-not-converging"), "{argv}");
        assert_eq!(fs::read_to_string(d.path().join("considered")).unwrap().trim(), "8 probes named no member");
    }

    struct Lc(std::sync::Mutex<Vec<(Vec<String>, Option<String>)>>);
    impl spira_store::lc_call::Transport for Lc {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push((argv.to_vec(), stdin.map(str::to_string)));
            let out = if argv[0] == "show-batch" { r#"{"progress":{"passes":2}}"# } else { "" };
            Ok(spira_store::lc_call::Reply { code: 0, out: out.into(), err: String::new(), read_ended: false })
        }
    }

    #[test_id(tuco7uf)]
    #[test]
    fn the_attribution_progress_is_written_beside_the_pass_counts_already_stored() {
        let lc = std::sync::Arc::new(Lc(Default::default()));
        let client = spira_store::lc_call::LcClient::over(lc.clone(), false);
        batch_progress_merge_via(&client, "r1", |mut p| {
            p["attribution"] = serde_json::json!({"probes": 3});
            p
        })
        .unwrap();
        let calls = lc.0.lock().unwrap();
        assert_eq!(calls[1].0, ["batch-progress", "r1"]);
        let body: serde_json::Value = serde_json::from_str(calls[1].1.as_deref().unwrap()).unwrap();
        assert_eq!(body["passes"], 2);
        assert_eq!(body["attribution"]["probes"], 3);
    }

    #[test_id(tlzoa7b)]
    #[test]
    fn an_already_ended_round_clears_its_refusal_record() {
        let d = testkit::TempDir::new("abandon-hold-ended");
        let e = queue_stub(d.path());
        let r = repo("alpha");
        assert!(!round_abandon(&e, &r, "r1", "red", "ci-red"));
        let files = crate::pace::Files::new(&e.run, "alpha");
        assert!(files.abandon_refused().is_some(), "control: the refusal was recorded");
        spira_config::round_ended::mark_abandoned(&e.run, "r1");
        assert!(round_abandon(&e, &r, "r1", "red", "ci-red"));
        assert!(files.abandon_refused().is_none());
    }

    #[test_id(t3itpu6)]
    #[test]
    fn a_refused_round_that_was_abandoned_by_hand_stops_holding() {
        let d = testkit::TempDir::new("abandon-hold-hand");
        let e = queue_stub(d.path());
        let r = repo("alpha");
        assert!(!round_abandon(&e, &r, "r1", "red", "ci-red"));
        fs::remove_file(d.path().join("live")).unwrap();
        assert_eq!(abandon_hold(&e, &r), None);
    }
    #[test_id(t5bmhli)]
    #[test]
    fn the_package_graph_holds_each_dir_relative_to_the_root_and_only_workspace_dependencies() {
        let meta = r#"{"workspace_root":"/w","packages":[
            {"name":"loom","manifest_path":"/w/loom/Cargo.toml","dependencies":[{"name":"runtime"},{"name":"serde"}]},
            {"name":"runtime","manifest_path":"/w/spira-runtime/Cargo.toml","dependencies":[]}]}"#;
        let g = package_graph(meta).unwrap();
        assert_eq!(g.dirs["runtime"], "spira-runtime");
        assert_eq!(g.deps["loom"], ["runtime"]);
        assert!(package_graph("not json").is_none());
    }

    #[test_id(tbm873w)]
    #[test]
    fn the_package_graph_maps_each_test_target_to_the_packages_that_have_it() {
        let meta = r#"{"workspace_root":"/w","packages":[
            {"name":"store","manifest_path":"/w/store/Cargo.toml","dependencies":[],"targets":[
                {"name":"store","kind":["lib"]},{"name":"conformance","kind":["test"]}]},
            {"name":"other","manifest_path":"/w/other/Cargo.toml","dependencies":[],"targets":[
                {"name":"conformance","kind":["test"]},{"name":"solo","kind":["test"]}]}]}"#;
        let g = package_graph(meta).unwrap();
        assert_eq!(g.tests["conformance"], ["store", "other"]);
        assert_eq!(g.tests["solo"], ["other"]);
        assert!(!g.tests.contains_key("store"));
    }

}

#[cfg(test)]
mod leftover_path_tests {
    use super::*;

    fn repo_with_commit(d: &Path) -> (Repo, String) {
        std::fs::create_dir_all(d).unwrap();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::write(d.join("f"), "a\n").unwrap();
        testkit::git(d, &["add", "."]);
        testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]);
        let sha = testkit::git(d, &["rev-parse", "HEAD"]).trim().to_string();
        let repo = Repo { name: "alpha".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        (repo, sha)
    }

    fn leftover(root: &Path) -> PathBuf {
        let wt = root.join("wt/.batcher-alpha");
        std::fs::create_dir_all(wt.join("target/release")).unwrap();
        std::fs::write(wt.join("target/release/spira-supervise"), "x").unwrap();
        wt
    }

    #[test_id(ta0ptym)]
    #[test]
    fn a_leftover_directory_at_the_worktree_path_is_cleared_and_the_outcome_says_so() {
        let root = testkit::TempDir::new("leftover-clear");
        let (repo, sha) = repo_with_commit(&root.path().join("r"));
        let wt = leftover(root.path());
        let cleared = worktree_reset(&repo, &wt, &sha).unwrap();
        assert!(cleared.is_some_and(|c| c.contains("not a worktree")), "the outcome names the clearing");
        assert!(wt.join(".git").exists(), "a worktree stands at the path now");
        assert!(!wt.join("target").exists(), "the leftover files are gone");
    }

    #[test_id(tezxcbg)]
    #[test]
    fn a_registered_worktree_is_reset_not_cleared() {
        let root = testkit::TempDir::new("leftover-registered");
        let (repo, sha) = repo_with_commit(&root.path().join("r"));
        let wt = root.path().join("wt/.batcher-alpha");
        assert_eq!(worktree_reset(&repo, &wt, &sha), Ok(None));
        std::fs::write(wt.join("kept-untracked"), "x").unwrap();
        assert_eq!(worktree_reset(&repo, &wt, &sha), Ok(None));
    }

    #[test_id(tikfwbz)]
    #[test]
    fn a_leftover_directory_a_process_works_in_is_refused_naming_the_process() {
        let root = testkit::TempDir::new("leftover-held");
        let (repo, sha) = repo_with_commit(&root.path().join("r"));
        let wt = leftover(root.path());
        let mut holder = Command::new("sleep").arg("30").current_dir(wt.join("target")).spawn().unwrap();
        let out = worktree_reset(&repo, &wt, &sha);
        holder.kill().unwrap();
        let _ = holder.wait();
        let err = out.unwrap_err();
        assert!(err.contains("sleep") && err.contains(&holder.id().to_string()), "{err}");
        assert!(wt.join("target/release/spira-supervise").exists(), "nothing was removed");
    }
}

#[cfg(test)]
mod config_key_tests {
    use super::*;

    fn commit(d: &Path, msg: &str) -> String {
        testkit::git(d, &["add", "-A"]);
        testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]);
        testkit::git(d, &["rev-parse", "HEAD"]).trim().to_string()
    }

    #[test_id(t8cwr6r)]
    #[test]
    fn a_tip_that_retires_a_fixture_key_and_adds_a_registry_key_reports_both_and_a_bystander_none() {
        let root = testkit::TempDir::new("config-keys");
        let d = root.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join("spira/conf.d")).unwrap();
        std::fs::create_dir_all(d.join("spira/testlib")).unwrap();
        std::fs::write(d.join("spira/testlib/complete.toml"), "[spira]\nkeep = 1\nretired_secs = 5\n").unwrap();
        std::fs::write(d.join("spira/conf.d/KEEP"), "x\n").unwrap();
        let base = commit(d, "base");
        testkit::git(d, &["checkout", "-q", "-b", "tip"]);
        std::fs::write(d.join("spira/testlib/complete.toml"), "[spira]\nkeep = 1\nnew_secs = 2\n").unwrap();
        std::fs::write(d.join("spira/conf.d/NEW_SECS"), "x\n").unwrap();
        let tip = commit(d, "tip");
        testkit::git(d, &["checkout", "-q", "main"]);
        testkit::git(d, &["checkout", "-q", "-b", "bystander"]);
        std::fs::write(d.join("other"), "x\n").unwrap();
        let by = commit(d, "other");
        let repo = Repo { name: "alpha".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        let (added, retired) = config_key_changes(&repo, &base, &tip);
        assert_eq!((added, retired), (vec!["NEW_SECS".to_string(), "new_secs".to_string()], vec!["retired_secs".to_string()]));
        assert_eq!(config_key_changes(&repo, &base, &by), (vec![], vec![]));
    }

    #[test_id(tila3fx)]
    #[test]
    fn a_tip_that_adds_a_scheduler_task_to_the_fixture_reports_no_config_key() {
        let root = testkit::TempDir::new("config-keys-task");
        let d = root.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join("spira/testlib")).unwrap();
        std::fs::write(d.join("spira/testlib/complete.toml"), "[spira]\nkeep = 1\ntask_modes = \"\"\"\nalpha=act\n\"\"\"\n").unwrap();
        let base = commit(d, "base");
        std::fs::write(d.join("spira/testlib/complete.toml"), "[spira]\nkeep = 1\ntask_modes = \"\"\"\nalpha=act\nbeta=act\n\"\"\"\n").unwrap();
        let tip = commit(d, "tip");
        let repo = Repo { name: "alpha".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        assert_eq!(config_key_changes(&repo, &base, &tip), (vec![], vec![]));
    }

    #[test_id(t3tb7o2)]
    #[test]
    fn gate_own_check_leaves_out_a_provisional_row_that_moves_an_owned_file_and_skips_an_ungated_ref() {
        let root = testkit::TempDir::new("gate-own-git");
        let d = root.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join("spira")).unwrap();
        std::fs::write(d.join("gate.steps"), "own spira/fence.sh\nstep true\n").unwrap();
        std::fs::write(d.join("spira/fence.sh"), "echo fence\n".repeat(20)).unwrap();
        commit(d, "base");
        testkit::git(d, &["checkout", "-q", "-b", "mover"]);
        testkit::git(d, &["mv", "spira/fence.sh", "elsewhere.sh"]);
        let mover = commit(d, "move");
        testkit::git(d, &["checkout", "-q", "main"]);
        testkit::git(d, &["checkout", "-q", "-b", "bystander"]);
        std::fs::write(d.join("other"), "x\n").unwrap();
        let by = commit(d, "other");
        let repo = Repo { name: "alpha".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        let row = |id: &str, tip: &str| (id.to_string(), tip.to_string(), 0u64, false, Vec::<String>::new(), true);
        let (kept, left) = gate_own_check(&repo, vec![row("sp-m", &mover), row("sp-b", &by)]).unwrap();
        assert_eq!(kept.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["sp-b"]);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].cause, Cause::GateUnjudged { paths: vec!["spira/fence.sh".into()] });
        testkit::git(d, &["checkout", "-q", "main"]);
        testkit::git(d, &["rm", "-q", "-f", "gate.steps"]);
        commit(d, "ungated");
        let (kept, left) = gate_own_check(&repo, vec![row("sp-m", &mover)]).unwrap();
        assert_eq!((kept.len(), left.len()), (1, 0), "a landing ref with no gate.steps skips the check");
    }

    #[test_id(tr6rj5n)]
    #[test]
    fn two_clean_merged_removed_lines_are_a_delta_that_does_not_parse_and_one_alone_is_not() {
        let root = testkit::TempDir::new("config-delta-merge");
        let d = root.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join("spira")).unwrap();
        let delta = d.join(CONFIG_DELTA);
        std::fs::write(&delta, "# head\n\n\n\n\n\n\n# tail\n\n[spira]\nk = 1\n").unwrap();
        commit(d, "base");
        testkit::git(d, &["checkout", "-q", "-b", "a"]);
        std::fs::write(&delta, "# head\nremoved = [\"x\"]\n\n\n\n\n\n\n# tail\n\n[spira]\nk = 1\n").unwrap();
        let a = commit(d, "a");
        testkit::git(d, &["checkout", "-q", "main"]);
        testkit::git(d, &["checkout", "-q", "-b", "b"]);
        std::fs::write(&delta, "# head\n\n\n\n\n\n\n# tail\nremoved = [\"y\"]\n\n[spira]\nk = 1\n").unwrap();
        let b = commit(d, "b");
        let repo = Repo { name: "alpha".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        let parses = |rev: &str| file_at(&repo, rev, CONFIG_DELTA).is_none_or(|t| spira_config::delta_added(&t).is_ok());
        assert!(parses(&a) && parses(&b), "each member alone parses");
        testkit::git(d, &["checkout", "-q", "a"]);
        testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-edit", &b]);
        assert!(!parses("HEAD"), "git merged both lines without a conflict, and the result is not TOML");
    }

    #[test_id(twj1x9f)]
    #[test]
    fn two_merged_fragments_are_a_delta_that_parses_with_both_retirements() {
        let root = testkit::TempDir::new("config-delta-fragments");
        let d = root.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join("spira")).unwrap();
        std::fs::write(d.join(CONFIG_DELTA), "[spira]\nk = 1\n").unwrap();
        commit(d, "base");
        let fragment = |branch: &str, key: &str| {
            testkit::git(d, &["checkout", "-q", "-b", branch, "main"]);
            std::fs::create_dir_all(d.join(CONFIG_DELTA_DIR)).unwrap();
            std::fs::write(d.join(CONFIG_DELTA_DIR).join(format!("{branch}.toml")), format!("removed = [\"{key}\"]\n")).unwrap();
            commit(d, branch)
        };
        let (a, b) = (fragment("a", "spira.x"), fragment("b", "spira.y"));
        let repo = Repo { name: "alpha".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
        testkit::git(d, &["checkout", "-q", "a"]);
        testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-edit", &b]);
        let text = config_delta_at(&repo, "HEAD").unwrap().unwrap();
        assert!(spira_config::delta_added(&text).is_ok() && text.contains("spira.x") && text.contains("spira.y"), "{text}");
        assert!(config_delta_at(&repo, &a).unwrap().unwrap().contains("spira.x"));
    }
}

#[cfg(test)]
mod eject_args_tests {
    use super::{eject_args, round_argv};

    #[test_id(t1qvuvz)]
    #[test]
    fn the_batchers_eject_names_the_batcher() {
        let a = eject_args("r9", "spira", "sp-a", "");
        let i = a.iter().position(|x| x == "--by").expect("--by");
        assert_eq!(a[i + 1], "batcher", "{a:?}");
    }

    #[test_id(tvckgo0)]
    #[test]
    fn the_batchers_eject_lets_the_queue_rebuild_the_rounds_tree() {
        let a = eject_args("r9", "spira", "sp-a", "lint");
        assert!(!a.iter().any(|x| x == "--no-rebuild"), "{a:?}");
    }

    #[test_id(tjwbdv1)]
    #[test]
    fn open_land_and_abandon_are_run_as_the_batcher_and_status_is_left_alone() {
        for verb in ["open", "land", "abandon"] {
            let a = round_argv(&[verb, "r9"]);
            assert_eq!(a, ["round", verb, "r9", "--by", "batcher"], "{a:?}");
        }
        assert_eq!(
            round_argv(&["status", "spira"]),
            ["round", "status", "spira"]
        );
        let e = round_argv(
            &eject_args("r9", "spira", "sp-a", "")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        assert_eq!(e.iter().filter(|x| *x == "--by").count(), 1, "{e:?}");
    }

    #[test_id(t3lcb66)]
    #[test]
    fn an_eject_naming_no_suite_sends_neither_red_nor_suites() {
        let a = eject_args("r9", "spira", "sp-a", "");
        assert!(!a.iter().any(|x| x == "--red" || x == "--suites"), "{a:?}");
    }

    #[test_id(tli6tv6)]
    #[test]
    fn an_eject_naming_a_suite_sends_it_as_suites() {
        let a = eject_args("r9", "spira", "sp-a", "test-b.sh");
        let i = a.iter().position(|x| x == "--suites").expect("--suites");
        assert_eq!(a[i + 1], "test-b.sh");
    }

    #[test_id(tvgahvt)]
    #[test]
    fn a_returned_members_reason_leads_with_the_mender_and_keeps_the_first_fail_line() {
        let fails = [("test-x.sh".to_string(), "boom at x.rs:9".to_string())];
        let lead = "Returned by the mender of test-x.sh in pass 2 of b-1: bad at x.rs:9";
        assert_eq!(crate::io::returned_lead(&["test-x.sh".into()], 2, "b-1", "bad at x.rs:9"), lead);
        let r = crate::io::eject_reason("sp-a", &["test-x.sh".into()], &fails, true, Some(lead));
        assert_eq!(r, format!("{lead} First FAIL, test-x.sh: boom at x.rs:9"));
        let plain = crate::io::eject_reason("sp-a", &["test-x.sh".into()], &fails, false, None);
        assert!(plain.contains("is stacked on a member that turned"), "{plain}");
    }
}

#[cfg(test)]
mod reap_attribution_tests {
    use super::*;

    #[test_id(t8fjfud)]
    #[test]
    fn reaping_takes_only_this_repositorys_attribution_branches() {
        let t = testkit::TempDir::new("batcher-cut-reap-attr");
        let d = t.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
        for b in ["spira/batcher-attr/r-1-b1", "spira/batcher-attr/r-1", "spira/batcher-attr/other-1"] {
            testkit::git(d, &["branch", b]);
        }
        let repo = Repo { name: "r".into(), path: d.to_path_buf(), base: "main".into(), forge: d.to_path_buf(), land: Land::Local };
        reap_attribution_branches("batcher", &repo);
        assert_eq!(testkit::git(d, &["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/"]), "spira/batcher-attr/other-1");
    }

    #[test_id(t46gbwr)]
    #[test]
    fn a_branch_git_will_not_delete_comes_back_with_its_reason() {
        let t = testkit::TempDir::new("batcher-cut-reap-fail");
        let d = t.path();
        testkit::git(d, &["init", "-q", "-b", "main"]);
        testkit::git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
        testkit::git(d, &["checkout", "-q", "-b", "spira/batcher-attr/r-1"]);
        let repo = Repo { name: "r".into(), path: d.to_path_buf(), base: "main".into(), forge: d.to_path_buf(), land: Land::Local };
        let (deleted, failed) = reap_branches(&repo, "spira/batcher-attr/r-*");
        assert!(deleted.is_empty());
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(!failed[0].1.is_empty());
    }
}

#[cfg(test)]
mod suites_in_tests {
    use super::*;

    #[test_id(tk5qh58)]
    #[test]
    fn suites_in_names_git_when_the_branch_is_missing_and_lists_the_tree_otherwise() {
        let d = testkit::TempDir::new("batcher-cut-suites-in");
        let g = |args: &[&str]| testkit::git(d.path(), args);
        g(&["init", "-q", "-b", "main"]);
        let err = suites_in(d.path(), "no-such-branch").unwrap_err();
        assert!(err.contains("git ls-tree"), "{err}");
        std::fs::create_dir_all(d.path().join("spira")).unwrap();
        std::fs::write(d.path().join("spira/test-a.sh"), "").unwrap();
        g(&["add", "-A"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]);
        assert_eq!(suites_in(d.path(), "main").unwrap(), ["test-a.sh"]);
    }
}

#[cfg(test)]
mod ref_file_tests {
    use super::*;

    fn sha(c: char) -> String {
        c.to_string().repeat(40)
    }

    /// A `.git` holding refs and no HEAD: git refuses it, so an answer here ran no git.
    fn headless(branches: &[(&str, &str)], packed: &str, base: &str) -> (testkit::TempDir, Repo) {
        let d = testkit::TempDir::new("io-ref-files");
        for (name, sha) in branches {
            let f = d.join(".git/refs/heads").join(name);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, format!("{sha}\n")).unwrap();
        }
        std::fs::create_dir_all(d.join(".git/refs/heads")).unwrap();
        std::fs::write(d.join(".git/packed-refs"), packed).unwrap();
        let repo = Repo { name: "r".into(), path: d.path().to_path_buf(), base: base.into(), forge: PathBuf::new(), land: Land::Local };
        (d, repo)
    }

    #[test_id(twzryts)]
    #[test]
    fn the_base_sha_is_read_from_a_loose_or_a_packed_ref() {
        let packed = format!("# pack-refs\n{} refs/heads/local/packed\n", sha('b'));
        let (_d, loose) = headless(&[("local/main", &sha('a'))], &packed, "local/main");
        assert_eq!(resolve_base_sha(&loose), Ok(sha('a')));
        let both = format!("{packed}{} refs/heads/local/main\n", sha('c'));
        let (_d, over) = headless(&[("local/main", &sha('a'))], &both, "local/main");
        assert_eq!(resolve_base_sha(&over), Ok(sha('a')), "a loose ref over a packed one");
        let (_d, packed_base) = headless(&[], &packed, "local/packed");
        assert_eq!(resolve_base_sha(&packed_base), Ok(sha('b')));
        let (_d, absent) = headless(&[], &packed, "local/gone");
        assert!(resolve_base_sha(&absent).is_err(), "an absent base is git's to refuse");
    }

    #[test_id(tjw0hvz)]
    #[test]
    fn branch_tips_are_read_loose_packed_and_loose_over_packed_by_bead_id() {
        let packed = format!("# pack-refs\n{} refs/heads/spira/sp-packed\n{} refs/heads/spira/sp-both\n{} refs/heads/main\n", sha('1'), sha('2'), sha('3'));
        let (d, repo) = headless(&[("spira/sp-loose", &sha('4')), ("spira/sp-both", &sha('5')), ("main", &sha('6'))], &packed, "main");
        let want = BTreeMap::from([("sp-loose".to_string(), sha('4')), ("sp-both".to_string(), sha('5')), ("sp-packed".to_string(), sha('1'))]);
        assert_eq!(repo_branch_tips(&repo), Ok(want));
        std::fs::write(d.join(".git/refs/heads/spira/sp-loose"), format!("{}\n", sha('7'))).unwrap();
        assert_eq!(repo_branch_tips(&repo).unwrap()["sp-loose"], sha('7'), "nothing is cached between reads");
    }
}

#[cfg(test)]
mod red_output_tests {
    use super::*;

    #[test_id(tynr80j)]
    #[test]
    fn a_red_pass_keeps_its_failing_blocks_where_the_next_pass_cannot_replace_them_and_the_beads_name_them() {
        let d = testkit::TempDir::new("io-red-output");
        let (pass, round) = (d.join("rounds/b.results"), d.join("batch-results/spira-r1"));
        fs::create_dir_all(&pass).unwrap();
        fs::write(pass.join("integration.json"), r#"{"stage":"integration-tests","verdict":"red","failed":["t::bad"]}"#).unwrap();
        fs::write(pass.join("integration-tests-failures.out"), "        FAIL [   0.010s] (2/4) fx t::bad\n    thread 't::bad' panicked at fx/src/t.rs:9:5:\n").unwrap();
        let kept = keep_red_test_output(&pass, &round.join(RED_PASS_DIR)).expect("a red pass is kept");
        fs::remove_dir_all(&pass).unwrap();
        assert_eq!(kept, round.join(RED_PASS_DIR));
        assert!(fs::read_to_string(kept.join("integration-tests-failures.out")).unwrap().contains("panicked at fx/src/t.rs:9:5"));
        let line = format!("{}: {}", "Red test output (each failing test's nextest block in *-failures.out)", kept.display());
        assert!(flip_body("test:tx", "spira/queue/1", Some(&kept)).contains(&line));
        assert!(red_output_line(Some(&kept)).ends_with(&kept.display().to_string()));
    }

    #[test_id(t3y6f5g)]
    #[test]
    fn a_green_pass_keeps_nothing_and_the_bead_says_none_was_kept() {
        let d = testkit::TempDir::new("io-red-output-green");
        let pass = d.join("pass");
        fs::create_dir_all(&pass).unwrap();
        fs::write(pass.join("unit.json"), r#"{"stage":"unit-tests","verdict":"green"}"#).unwrap();
        fs::write(pass.join("unit-tests.out"), "stale\n").unwrap();
        assert_eq!(keep_red_test_output(&pass, &d.join("red-pass")), None);
        assert!(!d.join("red-pass").exists());
        assert!(flip_body("test:tx", "b", None).contains("Red test output: none"));
    }
}
