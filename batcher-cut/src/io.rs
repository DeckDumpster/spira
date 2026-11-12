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
    let client = spira_config::lc_call::LcClient::for_bin(&bin.to_string_lossy())?
        .within(std::time::Duration::from_secs(env.lc_timeout));
    lc_answer(client.call(args))
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
    let Some(views) = store_view::shared() else {
        return lcq(env, &["show", id]);
    };
    match views.show(id).map_err(|e| format!("could not say: {e}"))?.value {
        Some(shown) => Ok(shown.to_string()),
        None => Err(format!("spira-lc exited 1: no row for {id}")),
    }
}

/// An answer as the `spira-lc` child's was read: its stdout when it applied; otherwise its exit
/// and the last line it said.
fn lc_answer(
    r: Result<spira_config::lc_call::Reply, spira_config::lc_call::LcError>,
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
        return views.rows(&q).map(|r| r.value).map_err(|e| format!("could not say: {e}"));
    }
    let mut held = env.lc_rows.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rows) = held.as_ref() {
        return Ok(rows.clone());
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&lcq(
        env,
        &["list", "--state", "SUBMITTED,CERTIFIED", "--snapshot"],
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
        return views.rows(&q).map(|r| r.value).map_err(|e| format!("could not say: {e}"));
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
    stack
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .filter(|(k, _)| !prereq_is_finished(env, k))
                .collect()
        })
        .unwrap_or_default()
}

/// A prerequisite the machine records as LANDED, SUPERSEDED or DROPPED sequences nothing.
/// An unreadable row is read as live: sequencing a member is the safe side.
fn prereq_is_finished(env: &Env, id: &str) -> bool {
    let Ok(out) = show_bead(env, id) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else {
        return false;
    };
    matches!(
        v.get("bead")
            .and_then(|b| b.get("state"))
            .and_then(|s| s.as_str()),
        Some("LANDED" | "SUPERSEDED" | "DROPPED")
    )
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

type PoolRow = (String, String, u64, bool, Vec<String>);

fn read_pool(env: &Env) -> Result<Vec<PoolRow>, String> {
    let mut pool: Vec<PoolRow> = Vec::new();
    {
        let rows = lc_rows(env)?;
        pool.extend(
            rows.iter()
                .filter(|r| in_pool_states(r))
                .filter(|r| holds_empty(r))
                .filter_map(|r| {
                    let id = r.get("bead_id")?.as_str()?.to_string();
                    let tip = r
                        .get("tip")
                        .and_then(|t| t.as_str())
                        .unwrap_or("none")
                        .to_string();
                    if r.get("ejected_red_tip").and_then(|t| t.as_str()) == Some(tip.as_str()) {
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
                    Some((id, tip, epoch, express, blocked_by))
                }),
        );
    }
    pool.sort();
    pool.dedup_by(|a, b| a.0 == b.0);
    Ok(pool)
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

/// Drops every member whose recorded stack names a barred bead, or a member dropped for that
/// reason, and says why on stderr.
fn drop_stacked_on_barred(
    members: Vec<Member>,
    mut barred: std::collections::BTreeSet<String>,
) -> Vec<Member> {
    let mut kept = members;
    loop {
        let Some(i) = kept
            .iter()
            .position(|m| m.stack.keys().any(|p| barred.contains(p)))
        else {
            return kept;
        };
        let m = kept.remove(i);
        let p = m
            .stack
            .keys()
            .find(|p| barred.contains(*p))
            .cloned()
            .unwrap_or_default();
        eprintln!("batcher: skipped {}: stacked on held {p}", m.id);
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
    let certified = read_pool(env)?;
    let branches = repo_branch_tips(repo)?;
    let mut barred = std::collections::BTreeSet::new();
    barred.extend(
        certified
            .iter()
            .filter(|(id, tip, ..)| !tip_current(tip, id, &branches))
            .map(|(id, ..)| id.clone()),
    );
    let certified: Vec<_> = certified
        .into_iter()
        .filter(|(id, tip, ..)| tip_current(tip, id, &branches))
        .collect();
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
    let mut out = Vec::new();
    for (id, tip, epoch, express, mut blocked_by) in certified {
        let Some((priority, title)) = by_id.get(&id) else {
            continue;
        };
        for dep in content_blockers.get(&id).into_iter().flatten() {
            if !blocked_by.contains(dep) && !prereq_is_finished(env, dep) {
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
        });
    }
    let stacked_on: Vec<String> = out
        .iter()
        .flat_map(|m| m.stack.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    barred.extend(barred_ids(env, &stacked_on)?);
    Ok(drop_stacked_on_barred(out, barred))
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
        .and_then(|r| spira_config::lc_state::parse_rows(&serde_json::Value::Array(r).to_string()))
    else {
        return fixes;
    };
    let lc = spira_config::lc_state::index(lc);
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
    let views = store_view::Projection::hot(spira_config::lc_call::LcClient::configured()?)?;
    views
        .batches_current()
        .map_err(|e| format!("could not say: {e}"))?;
    open_batch_of(views.batch_view(repo).map(|r| r.value))
}

/// The open batch a view's answer holds; a refused read could not say.
fn open_batch_of(
    read: Result<store_view::BatchView, spira_config::lc_call::LcError>,
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
    let holders = runtime::holders::holders(Path::new("/proc"), &here, &[std::process::id()]);
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
        if let Err(e) = migration_numbers::settle(wt) {
            eprintln!("batcher: refusing {id}: migrations after its merge: {e}");
            return MergeResult::Conflict;
        }
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
    spira_config::lifecycle_row::close_with(
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
    text.lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(str::to_string)
        .collect()
}

/// The config keys `tip` adds and retires since it forked from `base_sha`: the registry files
/// it creates or deletes, and the keys its test fixture config gains or loses.
pub fn config_key_changes(repo: &Repo, base_sha: &str, tip: &str) -> (Vec<String>, Vec<String>) {
    let git = |args: &[&str]| run(Command::new("git").arg("-C").arg(&repo.path).args(args), "git config keys");
    let (mut added, mut retired) = (Vec::new(), Vec::new());
    let range = format!("{base_sha}...{tip}");
    let status = git(&["diff", "--name-status", "--no-renames", &range, "--", CONF_REGISTRY]).unwrap_or_default();
    for l in status.lines() {
        match l.split_once('\t') {
            Some(("A", f)) => added.push(f.trim_start_matches(CONF_REGISTRY).to_string()),
            Some(("D", f)) => retired.push(f.trim_start_matches(CONF_REGISTRY).to_string()),
            _ => {}
        }
    }
    let fork = git(&["merge-base", base_sha, tip]).map(|m| m.trim().to_string()).unwrap_or_else(|_| base_sha.to_string());
    let show = |rev: &str| fixture_keys(&git(&["show", &format!("{rev}:{FIXTURE_CONFIG}")]).unwrap_or_default());
    let (before, after) = (show(&fork), show(tip));
    added.extend(after.difference(&before).cloned());
    retired.extend(before.difference(&after).cloned());
    (added, retired)
}

/// The paths `tip` changed since it forked from `base_sha` (`git diff base...tip`) — what
/// attribution orders suspects by and decides a rerun's build by.
pub fn changed_paths(repo: &Repo, base_sha: &str, tip: &str) -> Vec<String> {
    run(
        Command::new("git").arg("-C").arg(&repo.path).args([
            "diff",
            "--name-only",
            &format!("{base_sha}...{tip}"),
        ]),
        "git diff changed paths",
    )
    .unwrap_or_default()
    .lines()
    .filter(|l| !l.is_empty())
    .map(str::to_string)
    .collect()
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

pub fn set_branch(repo: &Repo, branch: &str, sha: &str) {
    let _ = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["branch", "-f", branch, sha])
        .status();
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

pub fn reap_branches(repo: &Repo, prefix: &str) {
    for b in local_branches(repo, prefix) {
        let _ = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["branch", "-D", &b])
            .env("SPIRA_REF_SANCTIONED", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
pub fn all_suites(repo: &Repo, branch: &str) -> Vec<String> {
    suites_in(&repo.path, branch)
}

pub fn suites_in(path: &Path, branch: &str) -> Vec<String> {
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
    )
    .unwrap_or_default();
    let mut suites: Vec<String> = out
        .lines()
        .filter_map(|l| l.rsplit('/').next())
        .filter(|name| name.starts_with("test-") && name.ends_with(".sh"))
        .map(str::to_string)
        .collect();
    suites.sort();
    suites.dedup();
    suites
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
    }
    Some(g)
}

#[cfg(test)]
mod local_red_ref_tests {
    use super::local_red_ref;

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

/// The saved output of each of `suites`, joined; a suite with none contributes nothing.
fn suite_outputs(results_dir: &Path, suites: &[String]) -> String {
    suites.iter().filter_map(|s| fs::read_to_string(locate(results_dir, s, "out")?).ok()).collect::<Vec<_>>().join("\n")
}

/// The failing tests and output tail of the suites a member turned red, recorded on the bead
/// for the builder's next brief. Best-effort: a refusal is said and never blocks the eject.
pub fn record_ejection(env: &Env, id: &str, tip: &str, files: &[String], results_dir: &Path, suites: &[String], fails: &[(String, String)]) {
    let mut output = suite_outputs(results_dir, suites);
    for (suite, line) in fails {
        output += &format!("\n{suite}: {line}");
    }
    let lc = |args: &[String]| lcq(env, &args.iter().map(String::as_str).collect::<Vec<_>>());
    if let Err(e) = spira_config::ejection::record(&lc, id, tip, files, &output, "batcher") {
        eprintln!("batcher: {id} failure not recorded on spira-lc: {e}");
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
pub fn suite_first_fail(results_dir: &Path, suite: &str) -> Option<String> {
    first_fail_line(&fs::read_to_string(locate(results_dir, suite, "out")?).ok()?)
}

/// Copies the round VM's results (every suite's `.out` and `.result`, nested run directories
/// included) into `<run>/rounds/<batch>.results`, where a red round's Concierge and
/// attribution step read them. Returns that directory.
pub fn keep_round_results(run: &Path, batch: &str, from: &Path) -> PathBuf {
    let to = run.join("rounds").join(format!("{batch}.results"));
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
    let _ = fs::remove_dir_all(&to);
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

/// Each red suite with the first FAIL line of its output in the round's results, written to
/// `<run>/rounds/<batch>.results/red-evidence` and appended to `why`, so an abandoned red round
/// can be attributed afterwards (law-a-failure-carries-its-own-explanation).
pub fn keep_red_evidence(run: &Path, batch: &str, why: &str, reds: &[String]) -> String {
    if reds.is_empty() || batch.is_empty() {
        return why.to_string();
    }
    let results = run.join("rounds").join(format!("{batch}.results"));
    let lines: Vec<String> = reds
        .iter()
        .map(|s| {
            format!(
                "{s}: {}",
                suite_first_fail(&results, s)
                    .unwrap_or_else(|| "(no FAIL line in its output)".into())
            )
        })
        .collect();
    let evidence = format!(
        "{why}\n{}\noutput: {}\n",
        lines.join("\n"),
        results.display()
    );
    if fs::create_dir_all(&results).is_ok() {
        let _ = fs::write(results.join("red-evidence"), &evidence);
    }
    evidence.trim_end().to_string()
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
            .env("SPIRA_INCIDENT_CAUSE", "local-round-red"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    filed_id(&out)
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

/// Repairs the integration breaks that are mechanical, on the round tree, as one commit
/// naming every member: a stale test-plan matrix is regenerated and a newly added spira
/// script is made executable. Returns what it fixed; empty means the tree was left untouched.
pub fn integration_fix(
    env: &Env,
    wt: &Path,
    base_sha: &str,
    members: &[Member],
) -> Result<Vec<&'static str>, String> {
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
        return Ok(fixes);
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
    Ok(fixes)
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
            let path = format!("/shots/{bead}/{tip}/{n}");
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
        si.write_all(stdin.as_bytes())
            .map_err(|e| format!("{what}: stdin: {e}"))?;
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

fn round_verb_with(
    env: &Env,
    args: &[&str],
    stdin: &str,
    vars: &[(&str, &str)],
) -> Result<VerbRun, String> {
    let all: Vec<&str> = std::iter::once("round")
        .chain(args.iter().copied())
        .collect();
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
    let reason = format!(
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
    let csv = if owner {
        suites.join(",")
    } else {
        String::new()
    };
    round_eject_with(env, repo, batch, id, &reason, &csv)
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

/// Members of an open round whose lifecycle row carries a red gate verdict at the very tip the
/// round holds, as `(id, the row's mark)`: the gate already judged this tree red, so the round's
/// corpus need not rediscover it.
pub fn gate_red_marks(env: &Env, members: &[(String, String)]) -> Vec<(String, String)> {
    members
        .iter()
        .filter_map(|(id, tip)| {
            let row =
                serde_json::from_str::<serde_json::Value>(&lcq(env, &["show", id]).ok()?).ok()?;
            let bead = row.get("bead")?;
            let field = |k: &str| bead.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let mark = field("reason").strip_prefix(lifecycle::bead::GATE_RED_MARK)?;
            (field("state") == "IN_DELIVERY" && field("tip") == tip)
                .then(|| (id.clone(), mark.to_string()))
        })
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

fn round_eject_with(
    env: &Env,
    repo: &Repo,
    batch: &str,
    id: &str,
    reason: &str,
    csv: &str,
) -> Result<(), String> {
    let mut args = vec![
        "eject",
        batch,
        id,
        &repo.name,
        "--reason-file",
        "-",
        "--no-rebuild",
    ];
    if !csv.is_empty() {
        args.extend(["--suites", csv]);
    } else {
        args.push("--red");
    }
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
    if last.code == Some(0) {
        Ok(())
    } else {
        Err(verb_failed("queue round eject", &last))
    }
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

/// One round `queue round status` lists as open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenRound {
    pub id: String,
    pub members: Vec<String>,
    pub head: String,
    pub worktree: String,
    pub phase: String,
    pub red: bool,
    pub age_secs: u64,
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
fn open_round_of(r: &store_view::Round, now: u64) -> OpenRound {
    let get = |k: &str| r.record.get(k).unwrap_or_default();
    OpenRound {
        id: r.record.id.clone(),
        members: get("members").split_whitespace().map(|m| m.split(':').next().unwrap_or("").to_string()).collect(),
        head: get("head").to_string(),
        worktree: get("worktree").to_string(),
        phase: r.phase.clone(),
        red: !get("red").is_empty(),
        age_secs: get("opened").parse().map_or(0, |t: u64| now.saturating_sub(t)),
    }
}

fn open_rounds_of(out: &str) -> Vec<OpenRound> {
    let prefixes = std::iter::once(String::new()).chain((2..).map(|n| format!("round{n}_")));
    prefixes
        .map_while(|p| {
            let id = verb_value(out, &format!("{p}batch_id"))?;
            let get = |k: &str| verb_value(out, &format!("{p}{k}")).unwrap_or_default();
            Some(OpenRound {
                id,
                members: get("members")
                    .split_whitespace()
                    .map(|m| m.split(':').next().unwrap_or("").to_string())
                    .collect(),
                head: get("head"),
                worktree: get("worktree"),
                phase: if p.is_empty() {
                    verb_value(out, "phase").unwrap_or_default()
                } else {
                    get("phase")
                },
                red: !get("red").is_empty(),
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
    let verbs: [(&str, Vec<&str>); 2] = [("pass-start", vec!["pass-start", "--resume", batch, &repo.name]), ("suites-started", vec!["suites-started", batch, &repo.name])];
    for (name, args) in &verbs {
        match round_verb(env, args, "") {
            Ok(r) if r.code == Some(0) => {}
            Ok(r) => return eprintln!("batcher {}: {}", repo.name, verb_failed(&format!("queue round {name}"), &r)),
            Err(e) => return eprintln!("batcher {}: {e}", repo.name),
        }
    }
}

/// An attribution that ended without an outcome leaves its failure text on the batch as the
/// pass's incomplete verdict; a pass that already has its verdict ignores it.
pub fn round_attribution_failed(env: &Env, repo: &Repo, batch: &str, why: &str) {
    let reason = format!("the attribution leg failed: {why}");
    match round_verb(env, &["pass-verdict", "--verdict", "incomplete", "--reason", &reason, batch, &repo.name], "") {
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
    if spira_config::round_ended::is_ended(&env.run, batch) {
        println!("batcher {}: round {batch} has already ended — no second abandon", repo.name);
        let files = crate::pace::Files::new(&env.run, &repo.name);
        if files.abandon_refused().is_some_and(|(b, _, _)| b == batch) {
            files.clear_abandon_refused();
        }
        return true;
    }
    let considered = abandon_considered(cause, ejected);
    let vars = [("QUEUE_ABANDON_CONSIDERED", considered.as_str())];
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
    let body = judgement_body(&repo.name, j, members, evidence);
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
    bead_id(&out)
}

fn bead_id(out: &str) -> Result<String, String> {
    let start = out
        .find('{')
        .ok_or_else(|| format!("bead.sh file: no JSON in output: {out}"))?;
    let v: serde_json::Value = serde_json::from_str(&out[start..])
        .map_err(|e| format!("bead.sh file: unparsed output: {e}"))?;
    let v = match v {
        serde_json::Value::Array(a) => a
            .into_iter()
            .next()
            .ok_or("bead.sh file: empty JSON array")?,
        o => o,
    };
    v.get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("bead.sh file: no id in output: {out}"))
}

/// File the follow-up bead for a deleted flip and return its id.
pub fn file_flip_bead(
    env: &Env,
    repo: &Repo,
    suite: &str,
    round_branch: &str,
) -> Result<String, String> {
    let title = format!(
        "{}: {suite} flipped in a batcher round and was deleted — restore it fixed",
        repo.name
    );
    let body = format!(
        "{suite} was red in the round's corpus and green on every re-run alone (twice on the round tree, on the base, and on each member touching it), so the batcher deleted it from the round ({round_branch}) rather than send a flip to CI (law-a-test-that-flips-is-deleted).\n\nDeliverable: find the nondeterminism, fix it and re-add the suite.\n"
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("flip-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        // FORK-EDGE: bead.sh is the operator tool for filing; it stays
        Command::new("bead.sh")
            .args([
                "file",
                &title,
                "--for",
                "batcher",
                "--repo",
                &repo.name,
                "--harness-title",
                "--body-file",
            ])
            .arg(&tmp)
            .arg("--json"),
        "bead.sh file",
    );
    let _ = fs::remove_file(&tmp);
    bead_id(&out?)
}

/// Commit the deletion of `flips` (suite, follow-up bead) into the round worktree, naming
/// the follow-up beads in the message.
pub fn commit_flip_deletion(
    env: &Env,
    wt: &Path,
    flips: &[(String, String)],
) -> Result<(), String> {
    // EXTERNAL: date — the local calendar date; std has no local time zone
    let date = run(Command::new("date").arg("+%F"), "date")?;
    crate::flip::apply(wt, flips, date.trim())?;
    run(
        Command::new("git").arg("-C").arg(wt).args(["add", "-A"]),
        "git add",
    )?;
    let names: Vec<String> = flips.iter().map(|(s, b)| format!("{s} ({b})")).collect();
    let msg = format!("batcher: delete flipped suite(s) {}", names.join(", "));
    run(
        Command::new("git")
            .arg("-C")
            .arg(wt)
            .args([
                "-c",
                &format!("user.name={}", env.git_name),
                "-c",
                &format!("user.email={}", env.git_email),
            ])
            .args(["commit", "-q", "-m", &msg]),
        "git commit",
    )?;
    Ok(())
}

/// The checks a suite deletion can break: the plan-matrix and its orphan fence, where the
/// round tree has them. Err names what failed.
pub fn recheck_after_deletion(wt: &Path, base_sha: &str) -> Result<(), String> {
    for (script, args) in [
        ("plan-matrix.sh", vec![]),
        ("plan-lint.sh", vec!["--orphans", base_sha]),
    ] {
        let path = wt.join("spira").join(script);
        if !path.exists() {
            continue;
        }
        // FORK-EDGE: the round tree's own plan scripts (that tree's version, not the release's)
        run(
            Command::new("bash").arg(&path).args(&args).current_dir(wt),
            script,
        )?;
    }
    Ok(())
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
    #[test]
    fn a_busy_lock_is_its_exit_code_never_its_words() {
        assert!(lock_busy(Some(QUEUE_LOCK_BUSY_EXIT)));
        assert!(!lock_busy(Some(1)));
        assert!(!lock_busy(Some(0)));
        assert!(!lock_busy(None));
    }

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
        };
        let pool = || {
            vec![
                member("sp-q", Some("sp-p")),
                member("sp-r", Some("sp-q")),
                member("sp-s", None),
            ]
        };
        let held: std::collections::BTreeSet<String> = ["sp-p".to_string()].into();
        let kept: Vec<String> = drop_stacked_on_barred(pool(), held)
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(kept, ["sp-s"]);
        let lifted = drop_stacked_on_barred(pool(), Default::default());
        assert_eq!(lifted.len(), 3);
    }
}

#[cfg(test)]
mod result_path_tests {
    use super::*;

    fn tmpdir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("batcher-cut-results-{tag}"))
    }

    #[test]
    fn reads_result_one_level_down_under_the_batch_key() {
        let d = tmpdir("nested");
        fs::create_dir_all(d.join("batch-abc")).unwrap();
        fs::write(d.join("batch-abc/test-x.sh.result"), "ok 3s\\n").unwrap();
        assert_eq!(result_status(&d, "test-x.sh"), Some(true));
    }

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

    #[test]
    fn absent_result_is_not_final_and_never_green() {
        let d = tmpdir("absent");
        assert_eq!(result_status(&d, "test-z.sh"), None);
    }

    /// sp-gjx1b (testenv DESIGN.md §3.7): an undeclared SKIP/SKIP-REQ is reclassified to a
    /// plain `red` status — this reader keys on field 1 only, so it needs no change to block
    /// on it, exactly like any other red. A *declared* skip/skip-req still passes.
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

    #[test]
    fn a_pass_is_recorded_only_where_the_machine_accepts_it() {
        assert!(sifted_accepts("SUBMITTED") && sifted_accepts("CERTIFIED"));
        assert!(!sifted_accepts("REWORK") && !sifted_accepts(""));
    }
}

#[cfg(test)]
mod fail_line_tests {
    use super::*;

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
        let why = keep_red_evidence(run.path(), "b1", "judged red: VERDICT", &reds);
        let kept = fs::read_to_string(results.join("red-evidence")).unwrap();
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
        let kept = keep_round_results(run.path(), "b9", &corpus);
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
        );
        assert!(
            why.contains("test-red.sh: FAIL: it broke")
                && why.contains("test-nested.sh: FAIL: nested"),
            "{why}"
        );
        assert!(why.contains(kept.to_str().unwrap()), "{why}");
    }

    #[test]
    fn an_ejected_members_digest_is_built_from_its_red_suites_output_alone() {
        let d = testkit::TempDir::new("batcher-cut-ejection");
        fs::write(d.path().join("test-a.sh.out"), "---- t::boom stdout ----\nthread 't::boom' panicked at x/y.rs:7:1:\nit broke\n\nfailures:\n").unwrap();
        fs::write(d.path().join("test-green.sh.out"), "all fine\n").unwrap();
        let out = suite_outputs(d.path(), &["test-a.sh".to_string(), "test-missing.sh".to_string()]);
        let digest = spira_config::ejection::digest("t1", &[], &out);
        assert!(digest.contains("FAIL t::boom at x/y.rs:7") && digest.contains("it broke") && !digest.contains("all fine"), "{digest}");
    }

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
            suite_first_fail(&d, "test-a.sh").as_deref(),
            Some("FAIL: boom")
        );
        assert_eq!(suite_first_fail(&d, "test-missing.sh"), None);
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod land_exit_tests {
    use super::*;

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
    #[test]
    fn a_base_moved_land_refusal_is_read_from_its_exit() {
        assert!(land_base_moved(Some(LAND_BASE_MOVED_EXIT)));
        assert!(!land_base_moved(Some(1)) && !land_base_moved(Some(7)) && !land_base_moved(None));
    }

    /// sp-822b8l: the typed records of a refused open, one member each; other lines are not.
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

    #[test]
    fn a_round_of_the_stores_view_is_listed_as_round_status_lists_it() {
        let record = store_view::Record {
            id: "r2".into(),
            state: "CI_RUNNING".into(),
            fields: [("head", "bbb"), ("members", "sp-c:tc sp-d:td"), ("worktree", "/w2"), ("opened", "1000"), ("red", "test-x.sh")].map(|(k, v)| (k.to_string(), v.to_string())).to_vec(),
        };
        let round = store_view::Round { phase: "certifying".into(), record };
        assert_eq!(
            open_round_of(&round, 1090),
            OpenRound { id: "r2".into(), members: vec!["sp-c".into(), "sp-d".into()], head: "bbb".into(), worktree: "/w2".into(), phase: "certifying".into(), red: true, age_secs: 90 }
        );
    }

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
        assert!(open_rounds_of("round=none\nrepo=spira\n").is_empty());
    }
}

#[cfg(test)]
mod wait_lock_tests {
    use super::*;

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

    #[test]
    fn touch_refreshes_a_stale_beat_file() {
        let d = testkit::TempDir::new("batcher-cut-beat2");
        let p = d.path().join("b");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        fs::File::create(&p).unwrap().set_modified(old).unwrap();
        touch(&p);
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
        round_slots: None,
        hypervisor: Hypervisor {
            host_mib: 0,
            round_vm_mib: 0,
            runner_mib: 0,
        },
        poll: std::time::Duration::from_secs(1),
        maxpar: 1,
        wall_secs: 1,
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

    #[test]
    fn two_round_vms_and_a_runner_must_fit() {
        assert!(hv(2 * 65536 + 16384, 65536).fits_a_staged_round());
        assert!(!hv(2 * 65536 + 16383, 65536).fits_a_staged_round());
    }

    #[test]
    fn an_unknown_size_never_fits() {
        assert!(!hv(0, 65536).fits_a_staged_round());
        assert!(!hv(1 << 30, 0).fits_a_staged_round());
    }
}

#[cfg(test)]
mod shots_tests {
    use super::*;

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

    #[test]
    fn a_declared_forge_script_is_a_stand_in_run_with_bash_and_the_default_runs_in_process() {
        let dir = testkit::TempDir::new("batcher-forge-stand-in");
        let script = dir.path().join("forge-fixture.sh");
        std::fs::write(&script, "echo \"$1 $2 $3\"\nexit 0\n").unwrap();
        let repo = Repo {
            name: "r".into(),
            path: PathBuf::from("/tmp/r"),
            base: "local/main".into(),
            forge: script,
            land: Land::Forge,
        };
        let r = forge_run(&repo, "pr-arm-queue", &["merge"]).unwrap();
        assert_eq!((r.code, r.out.as_str()), (0, "pr-arm-queue /tmp/r merge\n"));
        assert_eq!(
            forge::client::route_of(Path::new("forge")),
            forge::client::Route::InProcess
        );
    }

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

    #[test]
    fn a_lifecycle_answer_reads_as_the_childs_did() {
        use spira_config::lc_call::{LcError, Reply};
        assert_eq!(
            lc_answer(Ok(Reply {
                code: 0,
                out: "[]".into(),
                err: String::new()
            })),
            Ok("[]".into())
        );
        assert_eq!(
            lc_answer(Ok(Reply {
                code: 3,
                out: "x\nrefused: held\n".into(),
                err: String::new()
            })),
            Err("spira-lc exited 3: refused: held".into())
        );
        assert!(lc_answer(Err(LcError::Fault("cannot tell: down".into())))
            .unwrap_err()
            .contains("cannot tell"));
    }

    #[test]
    fn an_open_batch_the_store_cannot_vouch_for_could_not_say() {
        use spira_config::lc_call::LcError;
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

    #[test]
    fn a_ci_red_names_the_suites_the_repos_testenv_declares_not_a_literal_ci() {
        let d = tree(Some("[tier.gate]\ncommand = \"true\"\n"));
        assert_eq!(ci_red_suites(d.path()), vec!["gate".to_string()]);
    }

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

    #[test]
    fn a_refused_round_that_was_abandoned_by_hand_stops_holding() {
        let d = testkit::TempDir::new("abandon-hold-hand");
        let e = queue_stub(d.path());
        let r = repo("alpha");
        assert!(!round_abandon(&e, &r, "r1", "red", "ci-red"));
        fs::remove_file(d.path().join("live")).unwrap();
        assert_eq!(abandon_hold(&e, &r), None);
    }
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

    #[test]
    fn a_registered_worktree_is_reset_not_cleared() {
        let root = testkit::TempDir::new("leftover-registered");
        let (repo, sha) = repo_with_commit(&root.path().join("r"));
        let wt = root.path().join("wt/.batcher-alpha");
        assert_eq!(worktree_reset(&repo, &wt, &sha), Ok(None));
        std::fs::write(wt.join("kept-untracked"), "x").unwrap();
        assert_eq!(worktree_reset(&repo, &wt, &sha), Ok(None));
    }

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
}
