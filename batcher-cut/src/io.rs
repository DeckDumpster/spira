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
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration};

use batcher::core::{Member, MergeResult};

/// Where a repo's landed branch goes — `repo_land`'s `queue`/`queue.forge` alias normalizes
/// to `Forge`; `queue.local` is `Local`. Carried on `Repo` so a caller never has to re-derive
/// it from the base ref's own spelling (see `push_branch`).
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
        self.host_mib > 0 && self.round_vm_mib > 0 && self.round_vm_mib.saturating_mul(2).saturating_add(self.runner_mib) <= self.host_mib
    }
}

#[derive(Clone, Debug)]
pub struct Env {
    /// The lifecycle rows one repo step reads: fetched once by `lc_rows`, dropped by `lc_forget`.
    pub lc_rows: std::sync::Arc<std::sync::Mutex<Option<Vec<serde_json::Value>>>>,
    pub home: PathBuf,
    pub run: PathBuf,
    pub queue_dir: PathBuf,
    pub db: Option<PathBuf>,
    pub bd: String,
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
    pub checks_bin: PathBuf,
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
    /// Pinned toolchain for --with-bins' build, same key release.yml's own pin reads
    /// (SPIRA_RELEASE_RUST_TOOLCHAIN) — one fact about which Rust this workspace builds
    /// under, not a second default that can drift from the first.
    pub rust_toolchain: String,
    pub git_name: String,
    pub git_email: String,
    pub lc_bin: Option<PathBuf>,
    pub lc_timeout: u64,
    /// `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}` — where the round's tree certificate goes
    /// (gate::cert, queue/DESIGN.md §8 D12), resolved exactly as the gate resolves it.
    pub verdicts: PathBuf,
    /// How many times `round_land` tries while another queue operation holds the repo's lock.
    pub land_lock_attempts: u32,
    pub land_lock_wait: Duration,
    /// The Concierge's cut-pause file; a landing clears it unless it starts with `hold:`. `None` is off.
    pub land_unpause: Option<PathBuf>,
}

fn now() -> u64 {
    spira_config::vtime::now_epoch()
}

fn run(cmd: &mut Command, what: &str) -> Result<String, String> {
    let o = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let err = err.lines().last().unwrap_or("").trim();
        return Err(format!(
            "{what} exited {}{}",
            o.status.code().unwrap_or(-1),
            if err.is_empty() { String::new() } else { format!(": {err}") }
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

fn run_status(cmd: &mut Command) -> bool {
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
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(r#". "$1" && shift && "$0" "$@""#).arg(func).arg(env.home.join("lib.sh")).args(args);
    // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own release's
    // bin/+spira/ on the CHILD's PATH, never only inherited — see inbox-triage's scar.
    cmd.envs(spira_config::release_env::child_path_env_for_process());
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
/// aeon (sp-oxwvc). Returns the rebase-stale binary's own exit code: 0 (rebased — mechanically or
/// cleanly — and re-certified at the new tip) and 1/2 (a real content conflict or a red
/// gate) all mean the script itself already did everything the caller would otherwise do —
/// certify and note it, or reopen the bead with the conflicting hunk or gate output quoted
/// and bump its requeue count. Only 3 (a branch it could not even attempt: busy, missing,
/// or a scratch-worktree failure) leaves the caller's own fallback bookkeeping to run, the
/// same as if this call had never been made. A code the script never documents (a crash, a
/// missing binary) is folded into 3 for the same reason — silence must fail closed to "an
/// aeon still sees this", never to "nobody did anything and the round moved on".
pub fn rebase_stale(env: &Env, repo_name: &str, id: &str) -> i32 {
    // batch-job: this runs whatever its caller names, as long as that takes
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
    let bin = env.lc_bin.as_ref().ok_or_else(|| "no spira-lc program".to_string())?;
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).args(args);
    run(&mut cmd, "spira-lc")
}

/// The SUBMITTED and CERTIFIED lifecycle rows, from one `spira-lc list --state` per repo step: the
/// pool and the sifted tips are filters of it. A row outside those states is read by id through
/// `lc_rows_of`, never by listing the store.
pub fn lc_rows(env: &Env) -> Result<Vec<serde_json::Value>, String> {
    let mut held = env.lc_rows.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rows) = held.as_ref() {
        return Ok(rows.clone());
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&lcq(env, &["list", "--state", "SUBMITTED,CERTIFIED"])?).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))?;
    *held = Some(rows.clone());
    Ok(rows)
}

pub fn lc_rows_of(env: &Env, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&lcq(env, &["list", "--ids", &ids.join(",")])?).map_err(|e| format!("spira-lc list --ids: unparsed reply: {e}"))
}

pub fn lc_forget(env: &Env) {
    *env.lc_rows.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn in_pool_states(r: &serde_json::Value) -> bool {
    matches!(r.get("state").and_then(|s| s.as_str()), Some("CERTIFIED" | "SUBMITTED"))
}

/// Is the machine there to answer? `spira-lc list`, parsed — the same
/// probe the queue crate makes.
pub fn lc_probe(env: &Env) -> Result<(), String> {
    lc_rows(env).map(|_| ())
}

/// Ensure a bead row exists for every member (never a shortcut to CERTIFIED — that
/// transition is sp-vd9dn's territory) then `spira-lc cut` a brand-new batch. On success
/// the batch's version is exactly the member count (cut's own `MemberAdded` events are
/// the only thing that advances it from 0) — returned so the caller can record it on the
/// open-batch file the same way sp-o7nbr.2's `_lc_cut_batch` does.
pub fn lc_cut_batch(env: &Env, repo: &str, batch_id: &str, head: &str, base: &str, members: &[(String, String)]) -> Option<String> {
    for (id, _) in members {
        let _ = lcq(env, &["create-bead", id]);
    }
    let members_s = members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(",");
    match lcq(env, &["cut", batch_id, "--repo", repo, "--head", head, "--base", base, "--members", &members_s, "--actor", "batcher"]) {
        Ok(_) => Some(members.len().to_string()),
        Err(e) => {
            eprintln!("batcher {repo}: LIFECYCLE: spira-lc cut refused for {batch_id} (the round proceeds without batch_id/version): {e}");
            None
        }
    }
}

/// `id`'s stack (design stacked-dependents-2026-09-28 §1: `{prereq_bead_id: certified_tip}`)
/// off the lifecycle machine's own bead row — `spira-lc show`. A row with no `stack` column
/// (a bead the machine has never seen) is unstacked; a failed read or an unparseable reply is
/// also read as unstacked — never a hard error a round must refuse over, since `lc_probe`
/// already proved the machine reachable — but is said loudly on stderr.
fn read_stack(env: &Env, id: &str) -> BTreeMap<String, String> {
    let out = match lcq(env, &["show", id]) {
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
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null),
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
    let Ok(out) = lcq(env, &["show", id]) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else { return false };
    matches!(v.get("bead").and_then(|b| b.get("state")).and_then(|s| s.as_str()), Some("LANDED" | "SUPERSEDED" | "DROPPED"))
}

// ---------------------------------------------------------------------------------------
// The round pool: the lifecycle machine's unheld SUBMITTED and CERTIFIED beads, narrowed to this repo, with
// title/priority from the bead store and express from the lifecycle row. Same construction as queue-watch's
// snapshot(), which this borrows from directly.
// ---------------------------------------------------------------------------------------

fn holds_empty(r: &serde_json::Value) -> bool {
    match r.get("holds") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::Array(a)) => a.is_empty(),
        Some(serde_json::Value::String(t)) => serde_json::from_str::<Vec<serde_json::Value>>(t).map(|a| a.is_empty()).unwrap_or(false),
        Some(_) => false,
    }
}

type PoolRow = (String, String, u64, bool, Vec<String>);

fn read_pool(env: &Env) -> Result<Vec<PoolRow>, String> {
    let mut pool: Vec<PoolRow> = Vec::new();
    {
        let rows = lc_rows(env)?;
        pool.extend(rows.iter().filter(|r| in_pool_states(r)).filter(|r| holds_empty(r)).filter_map(|r| {
            let id = r.get("bead_id")?.as_str()?.to_string();
            let tip = r.get("tip").and_then(|t| t.as_str()).unwrap_or("none").to_string();
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
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Some((id, tip, epoch, express, blocked_by))
        }));
    }
    pool.sort();
    pool.dedup_by(|a, b| a.0 == b.0);
    Ok(pool)
}

/// Beads that carry their commits into whatever stacks on them yet may not be batched
/// themselves: held, or ejected at their current tip.
fn barred_ids(env: &Env, stacked_on: &[String]) -> Result<std::collections::BTreeSet<String>, String> {
    let mut rows = lc_rows(env)?;
    let pooled: std::collections::BTreeSet<&str> = rows.iter().filter_map(|r| r.get("bead_id")?.as_str()).collect();
    let outside: Vec<String> = stacked_on.iter().filter(|id| !pooled.contains(id.as_str())).cloned().collect();
    rows.extend(lc_rows_of(env, &outside)?);
    Ok(rows
        .iter()
        .filter(|r| !holds_empty(r) || (r.get("ejected_red_tip").and_then(|t| t.as_str()).is_some() && r.get("ejected_red_tip") == r.get("tip")))
        .filter_map(|r| r.get("bead_id")?.as_str().map(str::to_string))
        .collect())
}

/// Drops every member whose recorded stack names a barred bead, or a member dropped for that
/// reason, and says why on stderr.
fn drop_stacked_on_barred(members: Vec<Member>, mut barred: std::collections::BTreeSet<String>) -> Vec<Member> {
    let mut kept = members;
    loop {
        let Some(i) = kept.iter().position(|m| m.stack.keys().any(|p| barred.contains(p))) else { return kept };
        let m = kept.remove(i);
        let p = m.stack.keys().find(|p| barred.contains(*p)).cloned().unwrap_or_default();
        eprintln!("batcher: skipped {}: stacked on held {p}", m.id);
        barred.insert(m.id);
    }
}

/// The description and notes of `id`, where a target and its verdict live — read through
/// lc-serve, never by spawning bd.
pub fn bd_show_text(env: &Env, id: &str) -> Result<String, String> {
    let rows = bd_show(env, &[id.to_string()])?;
    let row = rows.as_array().and_then(|a| a.first()).ok_or_else(|| format!("content-rows: no row for {id}"))?;
    let field = |k: &str| row.get(k).and_then(|v| v.as_str()).unwrap_or("");
    Ok(format!("{}\n{}", field("description"), field("notes")))
}

/// The content rows of `ids` (title, priority, description, notes, external_ref, dependencies as
/// `{depends_on_id, type}`), from `spira-lc content-rows --full`.
fn bd_show(env: &Env, ids: &[String]) -> Result<serde_json::Value, String> {
    if ids.is_empty() {
        return Ok(serde_json::Value::Array(vec![]));
    }
    let text = lcq(env, &["content-rows", "--full", "--ids", &ids.join(",")])?;
    serde_json::from_str(&text).map_err(|e| format!("content-rows: unparsed output: {e}"))
}

/// `repo`'s own `refs/heads/spira/*` tips, keyed by bead id — the same authority
/// `queue_certified_list` (lib.sh) uses to scope the machine's beads to one
/// repository, and (law-batcher-earns-the-round-by-parity) the batcher's own check that a
/// CERTIFIED record's tip is still the branch's live tip: a branch that moved since
/// certification is not this bead's member until it is re-certified at the new tip, whatever
/// batch.sh did or did not do first.
fn repo_branch_tips(repo: &Repo) -> Result<BTreeMap<String, String>, String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads/spira/*"]),
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
    barred.extend(certified.iter().filter(|(id, tip, ..)| !tip_current(tip, id, &branches)).map(|(id, ..)| id.clone()));
    let certified: Vec<_> = certified.into_iter().filter(|(id, tip, ..)| tip_current(tip, id, &branches)).collect();
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
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
        content_blockers.insert(id.to_string(), blocks_edges(&it));
        let priority = it.get("priority").and_then(|p| p.as_u64()).map(|p| p.min(9) as u8);
        let title = it.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let own_ref = it.get("external_ref").and_then(|x| x.as_str()).unwrap_or("");
        let text = format!("{title}\n{}", it.get("description").and_then(|d| d.as_str()).unwrap_or(""));
        texts.insert(id.to_string(), (own_ref.starts_with(&format!("basefail:{}:", repo.name)), text));
        by_id.insert(id.to_string(), (priority, title));
    }
    let fixes = base_fix_ids(env, repo, &texts);
    let mut out = Vec::new();
    for (id, tip, epoch, express, mut blocked_by) in certified {
        let Some((priority, title)) = by_id.get(&id) else { continue };
        for dep in content_blockers.get(&id).into_iter().flatten() {
            if !blocked_by.contains(dep) && !prereq_is_finished(env, dep) {
                blocked_by.push(dep.clone());
            }
        }
        let stack = read_stack(env, &id);
        let base_fix = fixes.contains(&id);
        let priority = if base_fix { Some(0) } else { *priority };
        out.push(Member { id, tip, title: title.clone(), priority, express, base_fix, certified_at: epoch, stack, blocked_by });
    }
    let stacked_on: Vec<String> = out.iter().flat_map(|m| m.stack.keys().cloned()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    barred.extend(barred_ids(env, &stacked_on)?);
    Ok(drop_stacked_on_barred(out, barred))
}

/// The ids a content row's `dependencies` block it on. The lifecycle mirror of an edge is
/// best-effort, so the content store is read too: a lagging mirror must not admit a member.
fn blocks_edges(row: &serde_json::Value) -> Vec<String> {
    row.get("dependencies")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|d| matches!(d.get("type").and_then(|t| t.as_str()), Some("blocks" | "blocked-by" | "depends-on")))
        .filter_map(|d| d.get("depends_on_id").and_then(|i| i.as_str()).map(str::to_string))
        .collect()
}

/// The certified ids that fix an open base-red bead of `repo`: a bead the basefail bead
/// depends on, one citing it by id, or one carrying the basefail external ref itself. A
/// failed lookup yields none — the lane is an acceleration, never a gate on the round.
fn base_fix_ids(env: &Env, repo: &Repo, texts: &BTreeMap<String, (bool, String)>) -> std::collections::BTreeSet<String> {
    // Only a member of the pool can be a fix, so an empty pool has nothing to ask bd about.
    if texts.is_empty() {
        return Default::default();
    }
    let mut fixes: std::collections::BTreeSet<String> = texts.iter().filter(|(_, (own, _))| *own).map(|(id, _)| id.clone()).collect();
    // The content store lists the base-red beads; whether one is still open is its lifecycle
    // row's — not terminal (sp-mve9i). A base-red bead with no row, or a machine that cannot
    // answer, accelerates nothing: the lane is never a gate.
    let Ok(text) = lcq(env, &["content-rows", "--external-contains", &format!("basefail:{}:", repo.name)]) else { return fixes };
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str::<serde_json::Value>(&text) else { return fixes };
    let basefail_ids: Vec<String> = rows.iter().filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect();
    let Ok(lc) = lc_rows_of(env, &basefail_ids).and_then(|r| spira_config::lc_state::parse_rows(&serde_json::Value::Array(r).to_string())) else { return fixes };
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
            for d in b.get("dependencies").and_then(|d| d.as_array()).into_iter().flatten() {
                let blocks = d.get("type").and_then(|t| t.as_str()) != Some("parent-child");
                if let (true, Some(id)) = (blocks, d.get("depends_on_id").and_then(|i| i.as_str())) {
                    fixes.insert(id.to_string());
                }
            }
        }
    }
    fixes.retain(|id| texts.contains_key(id));
    fixes
}

// ---------------------------------------------------------------------------------------
// The open-batch record: exactly the key=value shape queue verdict (and queue-watch) read.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct OpenBatch {
    pub pr: String,
    pub head: String,
    pub base: String,
    pub members: Vec<(String, String)>,
    pub branch: String,
    pub opened: String,
    /// "batcher" when this record was written by this crate; empty for a record batch.sh
    /// wrote (batch.sh never writes this key, so its absence IS the legacy owner — no
    /// separate default to keep in sync). verdict.sh read it to decide whether a CI red
    /// on this PR was its own attribution's or the summoned batcher persona's (sp-lomk3);
    /// `queue verdict` sends every red to the persona (queue/DESIGN-verdict.md D1).
    pub owner: String,
    /// spira-lc's own key for this batch and its current CAS version (sp-o7nbr.4, same
    /// field names as sp-o7nbr.2's `_lc_cut_batch` writes for batch.sh) — present only
    /// when `spira-lc cut`/`stack` actually succeeded. queue verdict's lifecycle land walk
    /// (queue/DESIGN-verdict.md D3) reads these generically off this same open-batch file
    /// regardless of which cutter wrote it; absent means skip, not CAS against nothing.
    pub batch_id: String,
    pub version: String,
}

fn open_file(env: &Env, repo: &str) -> PathBuf {
    env.queue_dir.join(repo).join("open")
}

fn parse_kv(text: &str) -> BTreeMap<String, String> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

fn parse_members(s: &str) -> Vec<(String, String)> {
    s.split_whitespace()
        .map(|t| match t.split_once(':') {
            Some((id, tip)) => (id.to_string(), tip.to_string()),
            None => (t.to_string(), String::new()),
        })
        .collect()
}

pub fn read_open_batch(env: &Env, repo: &str) -> Result<Option<OpenBatch>, String> {
    let p = open_file(env, repo);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let kv = parse_kv(&text);
    let pr = kv.get("pr").cloned().unwrap_or_default();
    if pr.is_empty() {
        return Ok(None);
    }
    Ok(Some(OpenBatch {
        pr,
        head: kv.get("head").cloned().unwrap_or_default(),
        base: kv.get("base").cloned().unwrap_or_default(),
        members: parse_members(kv.get("members").map(String::as_str).unwrap_or("")),
        branch: kv.get("branch").cloned().unwrap_or_default(),
        opened: kv.get("opened").cloned().unwrap_or_default(),
        owner: kv.get("owner").cloned().unwrap_or_default(),
        batch_id: kv.get("batch_id").cloned().unwrap_or_default(),
        version: kv.get("version").cloned().unwrap_or_default(),
    }))
}

pub fn write_open_batch(env: &Env, repo: &str, ob: &OpenBatch) -> Result<(), String> {
    let p = open_file(env, repo);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let members = ob.members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(" ");
    let mut body = format!(
        "pr={}\nhead={}\nbase={}\nmembers={}\nopened={}\nbranch={}\nowner={}\n",
        ob.pr, ob.head, ob.base, members, ob.opened, ob.branch, ob.owner
    );
    if !ob.batch_id.is_empty() && !ob.version.is_empty() {
        body.push_str(&format!("batch_id={}\nversion={}\n", ob.batch_id, ob.version));
    }
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

// ---------------------------------------------------------------------------------------
// The prepared round: built and proven on an open batch's head while that PR is in CI,
// opened only once it has landed. Never a push; the open record is not touched.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    pub head: String,
    /// The open batch's head the round was built on: a different parent means the round
    /// was proven against a tree that is no longer the one in CI.
    pub parent: String,
    pub members: Vec<(String, String)>,
    /// Every pool member the build considered, set aside or not: a pool that differs from
    /// this is a different round to build.
    pub inputs: Vec<(String, String)>,
    pub green: bool,
    pub seconds: u64,
}

pub const PREPARED_BRANCH: &str = "spira/queue-prepared";

fn prepared_file(env: &Env, repo: &str) -> PathBuf {
    env.queue_dir.join(repo).join("prepared")
}

pub fn read_prepared(env: &Env, repo: &str) -> Option<Prepared> {
    let kv = parse_kv(&fs::read_to_string(prepared_file(env, repo)).ok()?);
    let head = kv.get("head").cloned().filter(|h| !h.is_empty())?;
    Some(Prepared {
        head,
        parent: kv.get("parent").cloned().unwrap_or_default(),
        members: parse_members(kv.get("members").map(String::as_str).unwrap_or("")),
        inputs: parse_members(kv.get("inputs").map(String::as_str).unwrap_or("")),
        green: kv.get("state").map(String::as_str) == Some("green"),
        seconds: kv.get("seconds").and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

pub fn write_prepared(env: &Env, repo: &str, p: &Prepared) -> Result<(), String> {
    let f = prepared_file(env, repo);
    if let Some(dir) = f.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let members = p.members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(" ");
    let inputs = p.inputs.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(" ");
    let body = format!(
        "head={}\nparent={}\nmembers={}\ninputs={}\nstate={}\nseconds={}\n",
        p.head, p.parent, members, inputs, if p.green { "green" } else { "red" }, p.seconds
    );
    let tmp = f.with_extension("tmp");
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &f).map_err(|e| format!("{}: {e}", f.display()))
}

pub fn clear_prepared(env: &Env, repo: &str) {
    let _ = fs::remove_file(prepared_file(env, repo));
}

/// True when `ancestor` is reachable from `descendant` — the prepared round descends from
/// the base the landed PR left.
pub fn is_ancestor(repo: &Repo, ancestor: &str, descendant: &str) -> bool {
    spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn subject_bead(subject: &str) -> Option<&str> {
    fn shaped(t: &str) -> Option<&str> {
        let (stem, rest) = t.split_once('-')?;
        let tail = rest.split('.').next()?;
        let ok = stem.len() >= 2
            && stem.bytes().all(|b| b.is_ascii_lowercase())
            && tail.len() >= 3
            && tail.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && rest.split('.').skip(1).all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        ok.then_some(t)
    }
    let lead = subject.split_once(':').map(|(a, _)| a.trim()).and_then(shaped);
    lead.or_else(|| subject.split_once("land ").and_then(|(_, r)| r.split_whitespace().next()).map(|t| t.trim_end_matches([':', ',', '.'])).and_then(shaped))
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
pub fn foreign_beads(repo: &Repo, base: &str, head: &str, members: &[String]) -> Result<Vec<String>, String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["log", "--format=%s", &format!("{base}..{head}")]),
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
    let out = run(Command::new("git").arg("-C").arg(&repo.path).args(["rev-parse", &repo.base]), "git rev-parse base")?;
    Ok(out.trim().to_string())
}

/// Fetch the base's remote so `repo.base` names what the remote holds now. A no-op under
/// `queue.local`, whose base is a local ref. A failed fetch is an error: building on an
/// unrefreshed base is the thing this exists to stop.
pub fn fetch_base(repo: &Repo) -> Result<(), String> {
    if repo.land == Land::Local {
        return Ok(());
    }
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
    run(Command::new("git").arg("-C").arg(&repo.path).args(["fetch", "-q", remote]), "git fetch base").map(|_| ())
}

/// Re-fetch and return the base the round is about to be opened on, refusing when `head`
/// does not descend from it — the corpus ran on an older base than the one the PR would
/// target, and what lands would be a tree no local run saw.
pub fn confirm_base(repo: &Repo, head: &str) -> Result<String, String> {
    fetch_base(repo)?;
    let base = resolve_base_sha(repo)?;
    if !run_status(Command::new("git").arg("-C").arg(&repo.path).args(["merge-base", "--is-ancestor", &base, head])) {
        return Err(format!("round does not descend from {} ({base}) — the base moved while the round was tested", repo.base));
    }
    Ok(base)
}

pub fn worktree_reset(repo: &Repo, wt: &Path, at_sha: &str) -> Result<(), String> {
    let _ = run(Command::new("git").arg("-C").arg(&repo.path).args(["worktree", "prune"]), "git worktree prune");
    if wt.join(".git").exists() {
        run(Command::new("git").arg("-C").arg(wt).args(["reset", "-q", "--hard", at_sha]), "git reset worktree")?;
        let _ = run(Command::new("git").arg("-C").arg(wt).args(["clean", "-qfd"]), "git clean worktree");
    } else {
        if let Some(parent) = wt.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        run(
            Command::new("git").arg("-C").arg(&repo.path).args(["worktree", "add", "-q", "--detach"]).arg(wt).arg(at_sha),
            "git worktree add",
        )?;
    }
    Ok(())
}

/// Is `tip` already an ancestor of the worktree's current HEAD? True for a member reset to
/// an old base, or with no commits of its own — `git merge` on such a tip succeeds as a
/// no-op ("Already up to date") even with `--no-ff`, which is indistinguishable from a real
/// merge by exit status alone, so this must be checked before `merge_member` ever shells out
/// to `git merge`.
fn tip_already_in_round(wt: &Path, tip: &str) -> bool {
    run_status(Command::new("git").arg("-C").arg(wt).args(["merge-base", "--is-ancestor", tip, "HEAD"]))
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
            .args(["-c", &format!("user.name={}", env.git_name), "-c", &format!("user.email={}", env.git_email)])
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
        let _ = run(Command::new("git").arg("-C").arg(wt).args(["merge", "--abort"]), "git merge --abort");
        MergeResult::Conflict
    }
}

/// The earliest of `winners` (id, tip) that touched one of `files` since `base_sha`: the
/// sibling a base-clean member lost its merge to, with the files they clashed on.
pub fn sibling_conflict(repo: &Repo, base_sha: &str, files: &[String], winners: &[(String, String)]) -> Option<(String, Vec<String>)> {
    for (id, tip) in winners {
        let o = spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(["diff", "--name-only", &format!("{base_sha}...{tip}")]).output().ok()?;
        let touched = String::from_utf8_lossy(&o.stdout).to_string();
        let shared: Vec<String> = files.iter().filter(|f| touched.lines().any(|l| l == f.as_str())).cloned().collect();
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
        .args(["merge-tree", "--write-tree", "--name-only", "--no-messages", side, tip])
        .output()
        .ok()?;
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let mut lines = out.lines();
    let first = lines.next().unwrap_or("");
    if o.status.code() != Some(1) || first.len() < 40 || !first.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(lines.take_while(|l| !l.is_empty()).map(str::to_string).collect())
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
    let listed = if files.is_empty() { "unknown".to_string() } else { files.join(", ") };
    let reason = format!("set aside for conflict with {} in {rounds} consecutive rounds; conflicting files: {listed}", repo.base);
    bead_reopen(env, id, "rebase-conflict", &format!("spira/{id} was {reason} — withdrawn by the batcher for rebase."));
    lc_withdraw(env, id);
    conflict_streak_clear(env, id);
}

/// A SUBMITTED member the round carried to a green corpus has earned CERTIFIED, so `land-local`
/// can walk it Deliver -> Delivered: the round's full suite is its trial, recorded as the gate
/// key. A refusal (tip moved, already certified) is said on stderr and never blocks the round.
pub fn lc_certify_round(env: &Env, members: &[(String, String)], round_key: &str) {
    for (id, tip) in members {
        let Ok(Ok(row)) = lcq(env, &["show", id]).map(|o| serde_json::from_str::<serde_json::Value>(&o)) else { continue };
        let bead = row.get("bead").cloned().unwrap_or_default();
        if bead.get("state").and_then(|s| s.as_str()) != Some("SUBMITTED") {
            continue;
        }
        let Some(version) = bead.get("version").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))) else { continue };
        let kind = serde_json::json!({"GatePass": {"tip": tip, "gate_key": format!("round:{round_key}")}}).to_string();
        if let Err(e) = lcq(env, &["event", "bead", id, "--expect", "SUBMITTED", "--version", &version.to_string(), "--actor", "batcher", "--kind", &kind]) {
            eprintln!("batcher: LIFECYCLE: {id} not certified by its round on spira-lc: {e}");
        }
    }
}

/// Takes a member the batcher withdraws out of the pool on the lifecycle machine (sp-mve9i).
/// The pool is the machine's CERTIFIED rows alone — bd's reopen no longer keeps a withdrawn
/// member out of the next round — so the withdrawal is the machine's too, by queue eject's
/// own route: `Deliver`, then `Returned(batch-ejected)`, to REWORK, where the builder's next
/// submit starts a fresh trial. A member not CERTIFIED there (resubmitted since, already
/// returned) is left alone; a refusal is said on stderr and never blocks the round.
pub fn lc_withdraw(env: &Env, id: &str) {
    let fail = |why: String| eprintln!("batcher: LIFECYCLE: {id} not returned to REWORK on spira-lc: {why}");
    let row = match lcq(env, &["show", id]).map(|o| serde_json::from_str::<serde_json::Value>(&o)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return fail(format!("show: unparsed reply: {e}")),
        Err(e) => return fail(format!("show: {e}")),
    };
    let bead = row.get("bead").cloned().unwrap_or_default();
    let mut state = bead.get("state").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let Some(mut version) = bead.get("version").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))) else {
        return fail("show: no version".into());
    };
    if state == "SUBMITTED" {
        let tip = bead.get("tip").and_then(|t| t.as_str()).unwrap_or("");
        let kind = serde_json::json!({"GateRed": {"tip": tip, "reason": "suites-failed"}}).to_string();
        if let Err(e) = lcq(env, &["event", "bead", id, "--expect", "SUBMITTED", "--version", &version.to_string(), "--actor", "batcher", "--kind", &kind]) {
            fail(format!("GateRed: {e}"));
        }
        return;
    }
    if state == "CERTIFIED" {
        let v = version.to_string();
        if let Err(e) = lcq(env, &["event", "bead", id, "--expect", "CERTIFIED", "--version", &v, "--actor", "batcher", "--kind", "\"Deliver\""]) {
            return fail(format!("Deliver: {e}"));
        }
        state = "IN_DELIVERY".into();
        version += 1;
    }
    if state != "IN_DELIVERY" {
        return;
    }
    let v = version.to_string();
    if let Err(e) = lcq(env, &["event", "bead", id, "--expect", "IN_DELIVERY", "--version", &v, "--actor", "batcher", "--kind", "{\"Returned\":{\"reason\":\"batch-ejected\"}}"]) {
        fail(format!("Returned: {e}"));
    }
}

/// `id`'s state on the machine.
pub fn lc_state(env: &Env, id: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(&lcq(env, &["show", id])?).map_err(|e| format!("show: unparsed reply: {e}"))?;
    v.get("bead").and_then(|b| b.get("state")).and_then(|s| s.as_str()).map(str::to_string).ok_or_else(|| "show: no state".to_string())
}

/// Sends `id` back to REWORK with `GateRed(reason)` at `tip`, but only while it is still the
/// SUBMITTED or CERTIFIED bead at that tip the screen judged.
pub fn lc_gate_red(env: &Env, id: &str, tip: &str, reason: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(&lcq(env, &["show", id])?).map_err(|e| format!("show: unparsed reply: {e}"))?;
    let bead = v.get("bead").cloned().unwrap_or_default();
    let state = bead.get("state").and_then(|s| s.as_str()).unwrap_or("");
    let version = bead.get("version").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).ok_or("show: no version")?;
    if !matches!(state, "SUBMITTED" | "CERTIFIED") || bead.get("tip").and_then(|t| t.as_str()) != Some(tip) {
        return Err(format!("{id} moved since screening (state {state})"));
    }
    let kind = serde_json::json!({"GateRed": {"tip": tip, "reason": reason}}).to_string();
    lcq(env, &["event", "bead", id, "--expect", state, "--version", &version.to_string(), "--actor", "sift", "--kind", &kind]).map(|_| ())
}

/// Records the screen's pass for `tip`; the machine refuses it unless `id` is still SUBMITTED or
/// CERTIFIED at that tip.
fn sifted_accepts(state: &str) -> bool {
    matches!(state, "SUBMITTED" | "CERTIFIED")
}

pub fn lc_sifted(env: &Env, id: &str, tip: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(&lcq(env, &["show", id])?).map_err(|e| format!("show: unparsed reply: {e}"))?;
    let bead = v.get("bead").cloned().unwrap_or_default();
    let state = bead.get("state").and_then(|s| s.as_str()).unwrap_or("");
    if !sifted_accepts(state) {
        println!("batcher: sift: {id} is {state}, not screened at {tip}; no pass recorded");
        return Ok(());
    }
    let version = bead.get("version").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).ok_or("show: no version")?;
    let kind = serde_json::json!({"Sifted": {"tip": tip}}).to_string();
    lcq(env, &["event", "bead", id, "--expect", state, "--version", &version.to_string(), "--actor", "sift", "--kind", &kind]).map(|_| ())
}

/// Bead id -> the tip the screen last passed, for every SUBMITTED and CERTIFIED bead that has one.
pub fn sifted_tips(env: &Env) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    {
        for r in lc_rows(env)?.into_iter().filter(in_pool_states) {
            let (Some(id), Some(tip)) = (r.get("bead_id").and_then(|x| x.as_str()), r.get("sifted_tip").and_then(|x| x.as_str())) else { continue };
            out.insert(id.to_string(), tip.to_string());
        }
    }
    Ok(out)
}

pub fn lc_supersede(env: &Env, id: &str, keeper: &str) -> Result<(), String> {
    let reason = format!("Sift: identical patch (patch-id) to {keeper}.");
    let bin = env.lc_bin.as_ref().ok_or_else(|| "no spira-lc program".to_string())?;
    spira_config::lifecycle_row::close_with(&bin.to_string_lossy(), id, &reason, "sift", Some(keeper))
}

/// The evidence a send-back stands on, written on the bead before the GateRed.
pub fn bead_note(env: &Env, id: &str, text: &str) -> Result<(), String> {
    // batch-job: bead.sh amend is a bead-store write that takes as long as the store does
    let mut cmd = Command::new("bash");
    cmd.arg(env.home.join("bead.sh")).args(["amend", id, "--note", text]).env("SPIRA_HOME", &env.home);
    run(&mut cmd, "bead.sh amend").map(|_| ())
}

/// Suites present at `member_tip` but gone from `base_sha` — reported only for a member set
/// aside on conflict, since that is the list its builder needs to know before rebasing.
pub fn deleted_suites(repo: &Repo, member_tip: &str, base_sha: &str) -> Vec<String> {
    let out = run(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["diff", "--diff-filter=D", "--name-only", member_tip, base_sha, "--", "spira/test-*.sh"]),
        "git diff deleted suites",
    )
    .unwrap_or_default();
    out.lines().filter(|l| !l.is_empty()).map(|l| l.rsplit('/').next().unwrap_or(l).to_string()).collect()
}

/// The paths `tip` changed since it forked from `base_sha` (`git diff base...tip`) — what
/// attribution orders suspects by and decides a rerun's build by.
pub fn changed_paths(repo: &Repo, base_sha: &str, tip: &str) -> Vec<String> {
    run(Command::new("git").arg("-C").arg(&repo.path).args(["diff", "--name-only", &format!("{base_sha}...{tip}")]), "git diff changed paths")
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn patch_id(repo: &Repo, base_sha: &str, tip: &str) -> Option<String> {
    let diff = spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(["diff", &format!("{base_sha}...{tip}")]).output().ok()?;
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
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or("").to_string())
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

pub fn push_branch(repo: &Repo, sha: &str, branch: &str) -> Result<(), String> {
    assert_eq!(repo.land, Land::Forge, "push_branch: unreachable under queue.local — it lands via queue land-local (sp-828tp), never a push");
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
    run(
        Command::new("git").arg("-C").arg(&repo.path).args(["push", "-q", remote]).arg(format!("{sha}:refs/heads/{branch}")),
        "git push",
    )
    .map(|_| ())
}

pub fn set_branch(repo: &Repo, branch: &str, sha: &str) {
    let _ = spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(["branch", "-f", branch, sha]).status();
}

pub fn local_branches(repo: &Repo, prefix: &str) -> Vec<String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["for-each-ref", "--format=%(refname:short)", &format!("refs/heads/{prefix}")]),
        "git for-each-ref",
    )
    .unwrap_or_default();
    out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
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
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn head_of(wt: &Path) -> Result<String, String> {
    Ok(run(Command::new("git").arg("-C").arg(wt).args(["rev-parse", "HEAD"]), "git rev-parse HEAD")?.trim().to_string())
}

/// The member ids `terminal_ready` (core) can treat as actually landed at `head`: every merge
/// commit in `base_sha..head` carries one parent — `merge_member`'s own `--no-ff` always makes
/// the merged tip the second parent, never rewritten by a conflict resolution — so a member
/// whose own certified tip is not among those parents never really merged, or was swapped for
/// a different patch after the corpus run that validated this round.
pub fn named_ids(repo: &Repo, base_sha: &str, head: &str, members: &[Member]) -> Vec<String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["log", "--merges", "--format=%P", &format!("{base_sha}..{head}")]),
        "git log merges",
    )
    .unwrap_or_default();
    let parents: std::collections::BTreeSet<&str> = out.split_whitespace().collect();
    members.iter().filter(|m| parents.contains(m.tip.as_str())).map(|m| m.id.clone()).collect()
}

/// Whether the round worktree `wt` holds the release binaries of `head`'s own tree — the
/// same rule `queue land-local --worktree` applies (queue::ops::land::round_bins): the
/// worktree's HEAD tree is `head`'s tree, and its target/release holds an executable.
/// round-vm installs the VM's release build there after checking the VM built this tree.
pub fn bins_present(repo: &Repo, wt: &Path, head: &str) -> bool {
    let Ok(want) = run(Command::new("git").arg("-C").arg(&repo.path).args(["rev-parse", &format!("{head}^{{tree}}")]), "git rev-parse tree")
    else {
        return false;
    };
    let Ok(have) = run(Command::new("git").arg("-C").arg(wt).args(["rev-parse", "HEAD^{tree}"]), "git rev-parse worktree tree") else {
        return false;
    };
    if want.trim() != have.trim() {
        return false;
    }
    let dir = wt.join("target").join("release");
    let Ok(rd) = fs::read_dir(&dir) else { return false };
    rd.flatten().any(|e| {
        let path = e.path();
        path.is_file() && fs::metadata(&path).map(|md| md.permissions().mode() & 0o111 != 0).unwrap_or(false)
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
        Command::new("git").arg("-C").arg(path).args(["ls-tree", "-r", "--name-only", branch, "--", "spira/"]),
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

/// The round-level pseudo-suite name a --with-bins build failure (exit 4) reports under —
/// never a real `test-*.sh` file, so it can't collide with one all_suites() would select.
pub const WORKSPACE_BUILD: &str = "workspace-build";

/// Same convention as WORKSPACE_BUILD, for a fence red (run_fences).
pub const GATE_FENCES: &str = "gate-fences";

/// Runs the repo's own gate command against `branch`, fences only (`SPIRA_GATE_SUITES=off`
/// — the same switch landing.sh's queue-mode certification already uses). Reuses gate.sh
/// itself rather than naming individual fence scripts here, so a fence added to the gate
/// is covered with nothing to keep in sync. On a non-zero exit, writes the combined output to
/// a file under `env.run` and returns its path as the error, for the incident this files.
pub fn run_fences(env: &Env, repo: &Repo, branch: &str) -> Result<(), FencesEnd> {
    // batch-job: runs a gate, build or forge script that takes as long as its work
    let out = Command::new("bash")
        .arg(env.home.join("gate.sh"))
        .arg(branch)
        .arg(&repo.name)
        .env("SPIRA_GATE_SUITES", "off")
        .output()
        .map_err(|e| FencesEnd::Refused(format!("gate.sh (fences): {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let dir = env.run.join("batch-results");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{}-{}-fences.log", repo.name, now()));
    let _ = fs::write(&path, &text);
    let at = path.display().to_string();
    if fences_judged_nothing(&text) {
        Err(FencesEnd::Refused(at))
    } else {
        Err(FencesEnd::Red(at))
    }
}

/// A gate killed at its wall cap judged nothing, however loaded the host made it; only a
/// gate that reached a verdict files a red.
pub fn fences_judged_nothing(text: &str) -> bool {
    text.contains("VERDICT=NO_VERDICT") || text.contains("VERDICT=FAIL reason=over-cap")
}

/// `Refused`: the gate judged nothing (NO_VERDICT, e.g. an unresolvable ref) — never a red.
#[derive(Debug, PartialEq, Eq)]
pub enum FencesEnd {
    Red(String),
    Refused(String),
}

/// Where a local pass for `tip` of `repo` is recorded: the file's existence is the record.
pub fn local_pass_path(env: &Env, repo: &Repo, tip: &str) -> PathBuf {
    env.run.join("local-pass").join(&repo.name).join(tip)
}

/// A forge repo's declared tiers (`testenv <rev> <repo>`, no `--suites`: the repo's own default,
/// compile and lint included) run in testenv on this host, once per tip. A green is recorded under
/// [`local_pass_path`]; a red returns the compiler/linter's `file:line` locations and the output tail.
pub fn local_tier(env: &Env, repo: &Repo, rev: &str) -> Result<(), String> {
    local_tier_keyed(env, repo, rev, local_pass_path(env, repo, rev))
}

/// Why a member's local check did not run: its tip does not merge onto the base (a conflict is
/// the merge's to report, never a tier red), or the merged tree could not be built at all.
#[derive(Debug, PartialEq, Eq)]
pub enum MemberTier {
    Conflict,
    Red(String),
}

pub fn member_pass_recorded(env: &Env, repo: &Repo, base_sha: &str, tip: &str) -> bool {
    local_pass_path(env, repo, &format!("{tip}@{base_sha}")).is_file()
}

/// The local check on the tree the batch will carry for this member: `tip` merged onto `base_sha`,
/// built in the object store with no worktree. The pass is recorded per (tip, base).
pub fn local_tier_member(env: &Env, repo: &Repo, base_sha: &str, tip: &str) -> Result<(), MemberTier> {
    let mark = local_pass_path(env, repo, &format!("{tip}@{base_sha}"));
    if mark.is_file() {
        return Ok(());
    }
    let git = |args: &[&str]| {
        Command::new("timeout")
            .args(["5", "git", "-C"])
            .arg(&repo.path)
            .args(["-c", "user.name=spira", "-c", "user.email=spira@localhost"])
            .args(args)
            .env("GIT_AUTHOR_DATE", "@0 +0000")
            .env("GIT_COMMITTER_DATE", "@0 +0000")
            .output()
    };
    let m = git(&["merge-tree", "--write-tree", base_sha, tip]).map_err(|e| MemberTier::Red(format!("git merge-tree: {e}")))?;
    if m.status.code() == Some(1) {
        return Err(MemberTier::Conflict);
    }
    if !m.status.success() {
        return Err(MemberTier::Red(format!("git merge-tree: {}", String::from_utf8_lossy(&m.stderr))));
    }
    let tree = String::from_utf8_lossy(&m.stdout).lines().next().unwrap_or("").trim().to_string();
    let c = git(&["commit-tree", &tree, "-p", base_sha, "-p", tip, "-m", &format!("member {tip} on {base_sha}")])
        .map_err(|e| MemberTier::Red(format!("git commit-tree: {e}")))?;
    if !c.status.success() {
        return Err(MemberTier::Red(format!("git commit-tree: {}", String::from_utf8_lossy(&c.stderr))));
    }
    let rev = String::from_utf8_lossy(&c.stdout).trim().to_string();
    local_tier_keyed(env, repo, &rev, mark).map_err(MemberTier::Red)
}

/// How a group check of several members ended short of green.
#[derive(Debug, PartialEq, Eq)]
pub enum GroupTier {
    /// This member's tip does not merge onto the group built so far; the merge reports it.
    Conflict(String),
    /// The merged head is red: its sha and the tier evidence.
    Red { head: String, evidence: String },
}

/// Where a red group head is recorded, so the same set is never checked as a group twice.
pub fn group_red_path(env: &Env, repo: &Repo, head: &str) -> PathBuf {
    env.run.join("local-red").join(&repo.name).join(head)
}

/// sp-cfni99: the merged head of `tips` onto `base_sha`, built in the object store with fixed
/// dates (so the same set on the same base is always the same sha), with no worktree.
pub fn group_head(repo: &Repo, base_sha: &str, tips: &[String]) -> Result<String, GroupTier> {
    let git = |args: &[&str]| {
        Command::new("timeout")
            .args(["5", "git", "-C"])
            .arg(&repo.path)
            .args(["-c", "user.name=spira", "-c", "user.email=spira@localhost"])
            .args(args)
            .env("GIT_AUTHOR_DATE", "@0 +0000")
            .env("GIT_COMMITTER_DATE", "@0 +0000")
            .output()
    };
    let red = |what: &str, e: String| GroupTier::Red { head: String::new(), evidence: format!("{what}: {e}") };
    let mut head = base_sha.to_string();
    for tip in tips {
        let m = git(&["merge-tree", "--write-tree", &head, tip]).map_err(|e| red("git merge-tree", e.to_string()))?;
        if m.status.code() == Some(1) {
            return Err(GroupTier::Conflict(tip.clone()));
        }
        if !m.status.success() {
            return Err(red("git merge-tree", String::from_utf8_lossy(&m.stderr).into_owned()));
        }
        let tree = String::from_utf8_lossy(&m.stdout).lines().next().unwrap_or("").trim().to_string();
        let c = git(&["commit-tree", &tree, "-p", &head, "-p", tip, "-m", &format!("group member {tip} on {base_sha}")])
            .map_err(|e| red("git commit-tree", e.to_string()))?;
        if !c.status.success() {
            return Err(red("git commit-tree", String::from_utf8_lossy(&c.stderr).into_owned()));
        }
        head = String::from_utf8_lossy(&c.stdout).trim().to_string();
    }
    Ok(head)
}

/// One local check of the group's merged head. Green records a pass for every member on
/// `base_sha` (so the batcher batches them all) and for the head itself; red records the head
/// as red and returns the evidence.
pub fn local_tier_group(env: &Env, repo: &Repo, base_sha: &str, tips: &[String]) -> Result<String, GroupTier> {
    let head = group_head(repo, base_sha, tips)?;
    let red_mark = group_red_path(env, repo, &head);
    if let Ok(evidence) = fs::read_to_string(&red_mark) {
        return Err(GroupTier::Red { head, evidence });
    }
    match local_tier_keyed(env, repo, &head, local_pass_path(env, repo, &format!("group-{head}"))) {
        Ok(()) => {
            for tip in tips {
                let mark = local_pass_path(env, repo, &format!("{tip}@{base_sha}"));
                if let Some(dir) = mark.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                let _ = fs::write(&mark, format!("{} group {head}\n", now()));
            }
            Ok(head)
        }
        Err(evidence) => {
            if let Some(dir) = red_mark.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(&red_mark, &evidence);
            Err(GroupTier::Red { head, evidence })
        }
    }
}

/// The files a red check's evidence names: its `locations:` line (`path:line:col`, comma
/// separated), path only.
pub fn red_locations(evidence: &str) -> Vec<String> {
    evidence
        .lines()
        .find_map(|l| l.strip_prefix("locations:"))
        .map(|rest| {
            rest.split(',')
                .filter_map(|loc| loc.trim().split(':').next().map(|p| p.trim().to_string()))
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn local_tier_keyed(env: &Env, repo: &Repo, rev: &str, mark: PathBuf) -> Result<(), String> {
    if mark.is_file() {
        return Ok(());
    }
    // batch-job: runs the repo's declared tiers, as long as they take
    let out = Command::new(&env.testenv_bin).arg(rev).arg(&repo.name).output().map_err(|e| format!("testenv: {e}"))?;
    if out.status.success() {
        if let Some(dir) = mark.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&mark, format!("{}\n", now()));
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Err(local_red_evidence(out.status.code(), &text))
}

/// Where a local red of the landing ref's own tiers is recorded, one file per ref sha.
pub fn local_fault_path(env: &Env, repo: &Repo, base: &str) -> PathBuf {
    env.run.join("local-fault").join(&repo.name).join(base)
}

/// A member's red is the member's only when the landing ref is green on the same tiers; a red
/// landing ref is a fault of the check. Returns that fault's evidence and whether this call
/// is the first to see it for `base`, so the alarm is raised once per landing-ref sha.
pub fn landing_ref_fault(env: &Env, repo: &Repo, base: &str) -> Option<(String, bool)> {
    let mark = local_fault_path(env, repo, base);
    if let Ok(evidence) = fs::read_to_string(&mark) {
        return Some((evidence, false));
    }
    let evidence = local_tier(env, repo, base).err()?;
    if let Some(dir) = mark.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&mark, &evidence);
    Some((evidence, true))
}

/// The recorded fault of the landing ref's own tiers for `base`, if one was seen. Reads only.
pub fn landing_ref_known_fault(env: &Env, repo: &Repo, base: &str) -> Option<(String, bool)> {
    fs::read_to_string(local_fault_path(env, repo, base)).ok().map(|e| (e, false))
}

fn local_red_evidence(rc: Option<i32>, text: &str) -> String {
    let mut sites: Vec<&str> = text.lines().filter_map(|l| l.trim().strip_prefix("--> ")).collect();
    sites.dedup();
    let tail: Vec<&str> = text.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{}\ntestenv exited {}\nlocations: {}\n{}", red_headline(text), rc.unwrap_or(-1), sites.join(", "), tail.join("\n"))
}

/// One line naming the tier that went red, its command and its first error, read from testenv's
/// own `RED` tier line, `command (<tier>):` line and `first error (<tier>):` line.
fn red_headline(text: &str) -> String {
    let tier = text.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        let name = w.next()?;
        (w.next() == Some("RED")).then(|| name.to_string())
    });
    let after = |prefix: String| text.lines().find_map(|l| l.trim().strip_prefix(prefix.as_str()).map(|r| r.trim().to_string()));
    let Some(tier) = tier else { return "red tier: unknown (testenv named none)".to_string() };
    let command = after(format!("command ({tier}):")).unwrap_or_else(|| "unknown".into());
    let first = after(format!("first error ({tier}):")).unwrap_or_else(|| "none reported".into());
    format!("red tier: {tier}; command: {command}; first error: {first}")
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
        let mtime = fs::metadata(&cand).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
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
pub fn eject_member(env: &Env, repo_name: &str, id: &str, suites: &[String], first_fails: &[(String, String)], owner: bool) {
    let suites_csv = suites.join(",");
    let note = format!(
        "Ejected by the merge queue's local attribution (pre-PR): spira/{id} {} red on: {}.{}",
        if owner { "turned" } else { "is stacked on a member that turned" },
        suites.join(", "),
        first_fails.iter().map(|(s, l)| format!(" First FAIL, {s}: {l}")).collect::<String>()
    );
    let cause = if owner { "queue-eject-local" } else { EJECT_COLLATERAL_CAUSE };
    let _ = lib_call(env, "bead_reopen", [id, cause, note.as_str(), suites_csv.as_str()]);
    lc_withdraw(env, id);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(env.run.join("landing.log")) {
        use std::io::Write;
        let _ = writeln!(f, "QUEUE LOCAL-EJECT {} repo={repo_name} id={id} suites={suites_csv}", now());
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
    let common = |c: &mut Command| {
        c.env("SPIRA_RUN", &env.run).envs(spira_config::release_env::child_path_env_for_process());
    };
    let record_args = |c: &mut Command| {
        c.args(["--member", &member.id, "--suite", suite, "--batch-id", batch_id, "--repo", &repo.name, "--paths", paths, "--evidence", evidence]);
    };
    if let Some(rc) = rerun_rc {
        let mut c = Command::new("escape-classify.sh");
        c.arg("rerun-verdict").args(["--rerun-rc", &rc.to_string()]);
        record_args(&mut c);
        common(&mut c);
        match run(&mut c, "escape-classify.sh rerun-verdict") {
            Ok(out) if out.contains("FLIP") => return format!("{suite} flipped on rerun with {} present — FLIP: flake recorded", member.id),
            Ok(_) => {}
            Err(e) => return format!("escape not recorded: {e}"),
        }
    }
    let mut c = Command::new("escape-classify.sh");
    c.arg("classify").args(["--repo".as_ref(), repo.path.as_os_str()]).args(["--base", base, "--tip", &member.tip]);
    c.arg("--suite-file").arg(wt.join("spira").join(suite));
    c.arg("--gate-log").arg(env.run.join("gate.log")).args(["--branch", &format!("spira/{}", member.id)]);
    common(&mut c);
    let class = match run(&mut c, "escape-classify.sh classify") {
        Ok(out) => out.trim().to_string(),
        Err(e) => return format!("escape not recorded: {e}"),
    };
    let mut c = Command::new("escape-classify.sh");
    c.arg("record").args(["--class", &class]);
    record_args(&mut c);
    common(&mut c);
    match run(&mut c, "escape-classify.sh record") {
        Ok(_) => format!("{} escaped {suite}: {class}", member.id),
        Err(e) => format!("escape not recorded: {e}"),
    }
}

#[cfg(test)]
mod local_red_ref_tests {
    use super::local_red_ref;

    #[test]
    fn key_is_bounded_and_distinguishes_detail() {
        let long = format!("landing-ref-red red tier: lint; command: {}", "x".repeat(2000));
        let s = vec!["local-tier".to_string()];
        let a = local_red_ref("dumpstar", &s, &long);
        assert!(a.len() <= 160, "{a}");
        assert!(a.starts_with("basefail-local:dumpstar:local-tier:landing-ref-red:"));
        assert_ne!(a, local_red_ref("dumpstar", &s, &format!("{long}y")));
        assert_eq!(a, local_red_ref("dumpstar", &s, &long));
    }
}

/// The first line of a suite's output that reports a failure, trimmed and bounded.
pub fn first_fail_line(out: &str) -> Option<String> {
    let l = out.lines().map(str::trim).find(|l| l.starts_with("FAIL") || l.contains("FAIL:") || l.starts_with("not ok"))?;
    Some(l.chars().take(300).collect())
}

/// The first FAIL line of `suite`'s output in the round's results, for an ejection's reason.
pub fn suite_first_fail(results_dir: &Path, suite: &str) -> Option<String> {
    first_fail_line(&fs::read_to_string(locate(results_dir, suite, "out")?).ok()?)
}

/// The incident key for a local red: a bounded identity plus a short hash of the full reason,
/// because external_ref is a fixed-width column and the reason carries command and error text.
pub fn local_red_ref(repo: &str, suites: &[String], reason: &str) -> String {
    let kind: String = reason.split_whitespace().next().unwrap_or("").chars().take(40).collect();
    let hash = reason.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    let suites: String = suites.join(",").chars().take(60).collect();
    format!("basefail-local:{repo}:{suites}:{kind}:{:08x}", hash & 0xffff_ffff)
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
    let title = format!("{}: local round red ({reason}) on {} — nothing can land", repo.name, suites.join(","));
    let body = batcher::core::base_fail_body(&repo.name, round_branch, suites, evidence);
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("local-red-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        Command::new("incident.sh").arg("file").arg(&title).arg(&tmp).env("SPIRA_INCIDENT_TYPE", "bug").env(
            "SPIRA_INCIDENT_PRIORITY",
            "1",
        ).env("SPIRA_INCIDENT_ACTOR", "batcher").env("SPIRA_INCIDENT_REPO", &repo.name).env(
            "SPIRA_INCIDENT_REF",
            local_red_ref(&repo.name, suites, reason),
        ).env("SPIRA_INCIDENT_CAUSE", "local-round-red"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    // THE LAST NON-EMPTY LINE, NOT THE WHOLE OUTPUT: incident.sh logs progress through the
    // same stdout it returns the id on (landing.sh's base_incident hits the same seam).
    let id = out.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string()).unwrap_or_default();
    if id.is_empty() {
        return Err(format!("incident.sh file: no id returned: {out}"));
    }
    Ok(id)
}

/// File an Ops incident for a landing whose release was not activated. The members landed, so
/// nothing is filed against them.
pub fn file_deploy_fault_incident(env: &Env, repo: &Repo, head: &str) -> Result<String, String> {
    let title = format!("{}: landed {head} but its release was not activated", repo.name);
    let body = format!(
        "queue land-local exited {LAND_DEPLOY_FAULT_EXIT} for {head}: the landing is recorded and every member is LANDED, but the release build, verify or activate failed and `current` was not moved. See the queue land-local stderr in the batcher log. Fix the release path; do not reopen the members."
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("deploy-fault-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        Command::new("incident.sh")
            .arg("file")
            .arg(&title)
            .arg(&tmp)
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", "1")
            .env("SPIRA_INCIDENT_ACTOR", "batcher")
            .env("SPIRA_INCIDENT_REPO", &repo.name)
            .env("SPIRA_INCIDENT_REF", format!("land-deploy-fault:{}:{head}", repo.name))
            .env("SPIRA_INCIDENT_CAUSE", "land-deploy-fault"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    out.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string()).ok_or_else(|| format!("incident.sh file: no id returned: {out}"))
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
    fs::write(&tmp, format!("key={key}\nbead={bead}\n")).map_err(|e| format!("{}: {e}", tmp.display()))?;
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
pub fn integration_fix(env: &Env, wt: &Path, base_sha: &str, members: &[Member]) -> Result<Vec<&'static str>, String> {
    let git = |args: &[&str]| run(spira_config::bounded::bounded("git").arg("-C").arg(wt).args(args), "git");
    let mut fixes = Vec::new();

    let added = git(&["diff", "--name-only", "--diff-filter=A", base_sha, "HEAD"])?;
    for path in added.lines().filter(|p| p.starts_with("spira/") && p.ends_with(".sh")) {
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
        run(Command::new("bash").arg(wt.join("spira/plan-matrix.sh")).current_dir(wt), "plan-matrix.sh")?;
        if !git(&["status", "--porcelain", "--", "docs/test-plan"])?.trim().is_empty() {
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
    c.arg("-C").arg(wt).args(["-c", &format!("user.name={}", env.git_name), "-c", &format!("user.email={}", env.git_email)]);
    c.args(["commit", "-q", "-F", "-"]).stdin(std::process::Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("git commit: {e}"))?;
    std::io::Write::write_all(&mut child.stdin.take().ok_or("git commit: no stdin")?, msg.as_bytes()).map_err(|e| e.to_string())?;
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("git commit of the integration fix failed".into());
    }
    Ok(fixes)
}

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

pub fn forge_pr_create(repo: &Repo, head: &str, base: &str, title: &str, body: &str) -> Result<String, String> {
    forge_pr_create_on(repo, head, base, title, body, None)
}

fn forge_cmd(repo: &Repo, path: Option<&std::ffi::OsStr>) -> std::process::Command {
    // A bare name (the default, `forge`, since sp-yv4b3) is the launcher-PATH program, run
    // directly; a configured SPIRA_FORGE path is run with bash, as batch.sh did.
    let mut cmd = if repo.forge.components().count() == 1 {
        spira_config::bounded::bounded(&repo.forge)
    } else {
        let mut c = spira_config::bounded::bounded("bash");
        c.arg(&repo.forge);
        c
    };
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    cmd
}

/// Arms auto-merge on a batch PR and requires the forge to have read the request back.
pub fn forge_pr_arm(repo: &Repo, pr: &str) -> Result<(), String> {
    forge_pr_arm_on(repo, pr, None)
}

fn forge_pr_arm_on(repo: &Repo, pr: &str, path: Option<&std::ffi::OsStr>) -> Result<(), String> {
    let mut cmd = forge_cmd(repo, path);
    cmd.arg("pr-automerge").arg(&repo.path).arg(pr).arg("merge");
    let o = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| format!("forge pr-automerge: {e}"))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("PR {pr} is open but auto-merge could not be armed (forge pr-automerge exited {})", o.status.code().unwrap_or(-1)))
    }
}

/// The reconciler: arms every open batch PR found without auto-merge.
pub fn forge_arm_queue(repo: &Repo) -> Result<(), String> {
    let mut cmd = forge_cmd(repo, None);
    cmd.arg("pr-arm-queue").arg(&repo.path).arg("merge");
    let o = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| format!("forge pr-arm-queue: {e}"))?;
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        println!("batcher {}: forge {l}", repo.name);
    }
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("forge pr-arm-queue exited {}", o.status.code().unwrap_or(-1)))
    }
}

/// `path`, when given, is the child's PATH — the seam tests use instead of mutating the process PATH.
fn forge_pr_create_on(repo: &Repo, head: &str, base: &str, title: &str, body: &str, path: Option<&std::ffi::OsStr>) -> Result<String, String> {
    let mut cmd = forge_cmd(repo, path);
    cmd.arg("pr-create").arg(&repo.path).arg(head).arg(base).arg(title);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("forge pr-create: {e}"))?;
    use std::io::Write;
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(body.as_bytes());
    }
    let o = child.wait_with_output().map_err(|e| format!("forge pr-create: {e}"))?;
    if !o.status.success() {
        return Err(format!("forge pr-create exited {}", o.status.code().unwrap_or(-1)));
    }
    let n = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string();
    if n.is_empty() {
        return Err("forge pr-create: no PR number returned".into());
    }
    Ok(n)
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

fn round_verb(env: &Env, args: &[&str], stdin: &str) -> Result<VerbRun, String> {
    use std::io::Write;
    // batch-job: this runs whatever its caller names, as long as that takes
    let mut cmd = Command::new(&env.queue_bin);
    cmd.arg("round").args(args);
    cmd.env("SPIRA_QUEUE_LOCK_HELD", "1");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let what = format!("queue round {}", args.first().copied().unwrap_or(""));
    let mut child = cmd.spawn().map_err(|e| format!("{what}: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(stdin.as_bytes()).map_err(|e| format!("{what}: stdin: {e}"))?;
    }
    let o = child.wait_with_output().map_err(|e| format!("{what}: {e}"))?;
    let run = VerbRun { code: o.status.code(), out: String::from_utf8_lossy(&o.stdout).to_string(), err: String::from_utf8_lossy(&o.stderr).to_string() };
    if !run.out.is_empty() {
        print!("{}", run.out);
    }
    if !run.err.is_empty() {
        eprint!("{}", run.err);
    }
    Ok(run)
}

fn verb_value(out: &str, key: &str) -> Option<String> {
    out.lines().find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')).map(|v| v.trim().to_string()))
}

fn verb_failed(what: &str, run: &VerbRun) -> String {
    format!("{what} exited {}: {}", run.code.map_or("by signal".to_string(), |c| c.to_string()), refusal_line(&run.err))
}

/// `queue round open` for `members` in `wt`: the batch id, once the verb admitted and merged
/// exactly the members asked for. Any other outcome is an Err and, when a round was opened, it
/// is abandoned first — the batcher never works a round it did not name.
pub fn round_open(env: &Env, repo: &Repo, wt: &Path, members: &[Member]) -> Result<String, String> {
    let text = members.iter().fold(String::new(), |mut acc, m| {
        acc.push_str(&format!("{}:{}\n", m.id, m.tip));
        acc
    });
    let wt_arg = wt.display().to_string();
    let run = round_verb(env, &["open", &repo.name, "--members-file", "-", "--worktree", &wt_arg], &text)?;
    if run.code != Some(0) {
        return Err(verb_failed("queue round open", &run));
    }
    let batch = verb_value(&run.out, "batch").ok_or("queue round open named no batch")?;
    let opened: Vec<String> = verb_value(&run.out, "members")
        .unwrap_or_default()
        .split_whitespace()
        .map(|m| m.split(':').next().unwrap_or("").to_string())
        .collect();
    let wanted: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    if opened != wanted {
        round_abandon(env, repo, &batch, "the round opened with members other than the ones the batcher named");
        return Err(format!("queue round open admitted [{}], the batcher named [{}] — round {batch} abandoned", opened.join(" "), wanted.join(" ")));
    }
    Ok(batch)
}

/// `queue round eject`: `owner` is a member whose own tip reddened `suites`; a collateral
/// member leaves uncharged. The batcher rebuilds the round tree itself, so the verb does not.
pub fn round_eject(env: &Env, repo: &Repo, batch: &str, id: &str, suites: &[String], first_fails: &[(String, String)], owner: bool) -> Result<(), String> {
    let reason = format!(
        "Ejected by the merge queue's local attribution (pre-PR): spira/{id} {} red on: {}.{}",
        if owner { "turned" } else { "is stacked on a member that turned" },
        suites.join(", "),
        first_fails.iter().map(|(s, l)| format!(" First FAIL, {s}: {l}")).collect::<String>()
    );
    let csv = if owner { suites.join(",") } else { String::new() };
    round_eject_with(env, repo, batch, id, &reason, &csv)
}

/// Members of an open round whose lifecycle row carries a red gate verdict at the very tip the
/// round holds, as `(id, the row's mark)`: the gate already judged this tree red, so the round's
/// corpus need not rediscover it.
pub fn gate_red_marks(env: &Env, members: &[(String, String)]) -> Vec<(String, String)> {
    members
        .iter()
        .filter_map(|(id, tip)| {
            let row = serde_json::from_str::<serde_json::Value>(&lcq(env, &["show", id]).ok()?).ok()?;
            let bead = row.get("bead")?;
            let field = |k: &str| bead.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let mark = field("reason").strip_prefix(lifecycle::bead::GATE_RED_MARK)?;
            (field("state") == "IN_DELIVERY" && field("tip") == tip).then(|| (id.clone(), mark.to_string()))
        })
        .collect()
}

/// `queue round eject` for a member the gate had already marked red while it sat in the round.
pub fn round_eject_gate_red(env: &Env, repo: &Repo, batch: &str, id: &str, mark: &str) -> Result<(), String> {
    let reason = format!(
        "Ejected from the round before its corpus ran: spira/{id}'s own gate went red at the tip the round holds ({mark}). The gate's whole output is kept under the gate-worker's output directory."
    );
    round_eject_with(env, repo, batch, id, &reason, "")
}

fn round_eject_with(env: &Env, repo: &Repo, batch: &str, id: &str, reason: &str, csv: &str) -> Result<(), String> {
    let mut args = vec!["eject", batch, id, &repo.name, "--reason-file", "-", "--no-rebuild"];
    if !csv.is_empty() {
        args.extend(["--suites", csv]);
    } else {
        args.push("--red");
    }
    let attempts = env.land_lock_attempts.max(1);
    let mut last = round_verb(env, &args, reason)?;
    for n in 1..attempts {
        if last.code == Some(0) || !last.err.contains(LOCK_BUSY) {
            break;
        }
        println!("batcher {}: queue round eject refused ({}) — retry {n}/{attempts}", repo.name, refusal_line(&last.err));
        std::thread::sleep(env.land_lock_wait);
        last = round_verb(env, &args, reason)?;
    }
    if last.code == Some(0) {
        Ok(())
    } else {
        Err(verb_failed("queue round eject", &last))
    }
}

/// `queue round certify --attest <head>`: the batcher's own corpus ran green on `head`, the
/// round worktree's head, so the verb records the round's GREEN for that tree and no other.
pub fn round_certify(env: &Env, repo: &Repo, batch: &str, head: &str) -> Result<(), String> {
    let run = round_verb(env, &["certify", batch, &repo.name, "--attest", head], "")?;
    if run.code == Some(0) {
        Ok(())
    } else {
        Err(verb_failed("queue round certify", &run))
    }
}

/// The member ids the round record holds now, as `queue round status` reports them.
pub fn round_members(env: &Env, repo: &Repo) -> Result<Vec<String>, String> {
    let run = round_verb(env, &["status", &repo.name], "")?;
    if run.code != Some(0) {
        return Err(verb_failed("queue round status", &run));
    }
    Ok(verb_value(&run.out, "members")
        .unwrap_or_default()
        .split_whitespace()
        .map(|m| m.split(':').next().unwrap_or("").to_string())
        .collect())
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
    Ok(Some(verb_value(&run.out, "batch_id").unwrap_or_default()))
}

/// The repo's local verdict as `write_local_verdict` recorded it: (verdict, at).
pub fn read_local_verdict(env: &Env, repo: &str) -> Option<(String, u64)> {
    let kv = parse_kv(&fs::read_to_string(env.queue_dir.join(repo).join("local-verdict")).ok()?);
    Some((kv.get("verdict")?.clone(), kv.get("at")?.parse().ok()?))
}

pub fn round_abandon(env: &Env, repo: &Repo, batch: &str, why: &str) {
    match round_verb(env, &["abandon", batch, &repo.name, "--reason-file", "-"], why) {
        Ok(r) if r.code == Some(0) => {}
        Ok(r) => println!("batcher {}: could not abandon round {batch}: {}", repo.name, verb_failed("queue round abandon", &r)),
        Err(e) => println!("batcher {}: could not abandon round {batch}: {e}", repo.name),
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
    let stem = env.run.join(format!("round-bg-{}-{}", std::process::id(), args.first().copied().unwrap_or("verb")));
    let (out, err) = (stem.with_extension("out"), stem.with_extension("err"));
    let file = |p: &Path| fs::File::create(p).map_err(|e| format!("{what}: {}: {e}", p.display()));
    // batch-job: this runs whatever its caller names, as long as that takes
    let mut cmd = Command::new(&env.queue_bin);
    cmd.arg("round").args(args);
    cmd.env("SPIRA_QUEUE_LOCK_HELD", "1");
    cmd.stdin(Stdio::piped()).stdout(file(&out)?).stderr(file(&err)?);
    let mut child = cmd.spawn().map_err(|e| format!("{what}: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(stdin.as_bytes()).map_err(|e| format!("{what}: stdin: {e}"))?;
    }
    Ok(BgVerb { child, what, out, err })
}

impl BgVerb {
    pub fn wait(mut self) -> Result<VerbRun, String> {
        let status = self.child.wait().map_err(|e| format!("{}: {e}", self.what))?;
        let read = |p: &Path| {
            let t = fs::read_to_string(p).unwrap_or_default();
            let _ = fs::remove_file(p);
            t
        };
        let run = VerbRun { code: status.code(), out: read(&self.out), err: read(&self.err) };
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
pub fn round_stage_spawn(env: &Env, repo: &Repo, wt: &Path, members: &[Member]) -> Result<BgVerb, String> {
    let text = members.iter().fold(String::new(), |mut acc, m| {
        acc.push_str(&format!("{}:{}\n", m.id, m.tip));
        acc
    });
    let wt_arg = wt.display().to_string();
    round_verb_bg(env, &["stage", &repo.name, "--members-file", "-", "--worktree", &wt_arg], &text)
}

/// The staged round's batch id once the verb wrote the row; any other end is `None` and the
/// open round goes on as if nothing had been staged.
pub fn round_stage_done(repo: &Repo, bg: BgVerb) -> Option<String> {
    match bg.wait() {
        Ok(run) if run.code == Some(0) => verb_value(&run.out, "batch"),
        Ok(run) => {
            println!("batcher {}: {} — no round staged", repo.name, verb_failed("queue round stage", &run));
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
            println!("batcher {}: {} — the staged round gets its own pass", repo.name, verb_failed("queue round stage-test", &run));
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

pub fn parse_promotion(out: &str) -> Option<Promotion> {
    let members = verb_value(out, "members")?
        .split_whitespace()
        .filter_map(|m| m.split_once(':').map(|(i, t)| (i.to_string(), t.to_string())))
        .collect();
    Some(Promotion { batch: verb_value(out, "batch")?, head: verb_value(out, "head")?, members, attested: verb_value(out, "attested").as_deref() == Some("1") })
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
            println!("batcher {}: {}", repo.name, verb_failed("queue round promote", &run));
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
}

pub const LOCK_BUSY: &str = "holds the lock";

fn refusal_line(err: &str) -> String {
    err.lines()
        .find(|l| l.contains(LOCK_BUSY))
        .or_else(|| err.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("(no stderr)")
        .trim()
        .to_string()
}

/// A refusal because another queue operation holds the lock is retried (bounded); every other
/// outcome is final.
pub fn round_land(env: &Env, repo: &Repo, batch: &str) -> Result<LandRun, String> {
    let attempts = env.land_lock_attempts.max(1);
    let mut last = LandRun { outcome: LandOutcome::Refused, refusal: String::new() };
    for n in 1..=attempts {
        let run = round_verb(env, &["land", batch, &repo.name], "")?;
        last = LandRun { outcome: classify_land_exit(run.code), refusal: refusal_line(&run.err) };
        let busy = last.outcome == LandOutcome::Refused && last.refusal.contains(LOCK_BUSY);
        if !busy || n == attempts {
            break;
        }
        println!("batcher {}: queue round land refused ({}) — retry {n}/{attempts}", repo.name, last.refusal);
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
    let Some(path) = env.land_unpause.as_ref() else { return Ok(Unpause::Off) };
    let Ok(text) = fs::read_to_string(path) else { return Ok(Unpause::Absent) };
    if text.starts_with("hold:") {
        return Ok(Unpause::Held);
    }
    fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Unpause::Cleared)
}

/// True only when `head` is an ancestor of the repo's landing ref: the one fact that says the
/// round landed, whatever land-local reported.
pub fn head_on_base(repo: &Repo, head: &str) -> bool {
    run_status(
        Command::new("git").arg("-C").arg(&repo.path).args(["merge-base", "--is-ancestor", head, &repo.base]),
    )
}

/// Alarm for a round that did not land: the incident names the refusal line.
pub fn file_land_unverified_incident(env: &Env, repo: &Repo, head: &str, detail: &str) -> Result<String, String> {
    let title = format!("{}: round head {head} did not reach {}", repo.name, repo.base);
    let body = format!(
        "queue land-local did not land round head {head} on {}: {detail}\nNo member was landed by this round; the batcher reported the round as refused. Find why land-local refused (its stderr is in the batcher log) before the next round.",
        repo.base
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("land-unverified-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        Command::new("incident.sh")
            .arg("file")
            .arg(&title)
            .arg(&tmp)
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", "1")
            .env("SPIRA_INCIDENT_ACTOR", "batcher")
            .env("SPIRA_INCIDENT_REPO", &repo.name)
            .env("SPIRA_INCIDENT_REF", format!("land-unverified:{}:{head}", repo.name))
            .env("SPIRA_INCIDENT_CAUSE", "land-unverified"),
        "incident.sh file",
    );
    let _ = fs::remove_file(&tmp);
    let out = out?;
    let id = out.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string()).unwrap_or_default();
    if id.is_empty() {
        return Err(format!("incident.sh file: no id returned: {out}"));
    }
    Ok(id)
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
    let mut cmd = spira_config::bounded::bounded(bin);
    cmd.arg("--family").arg(family).arg("--root").arg(&env.run);
    for (k, v) in fields {
        cmd.arg("--field-str").arg(format!("{k}={v}"));
    }
    let _ = cmd.status();
}

// ---------------------------------------------------------------------------------------
// Judgement: file a bead through bead.sh's own contract (never `bd create` directly) so the
// batcher persona's partition labels come from its fayth, the same way every other filed
// bead in this harness does (sp-47kq1).
// ---------------------------------------------------------------------------------------

/// File a judgement bead for the summoned batcher persona and return its id. The body is
/// written to a scratch file under `env.run/tmp` rather than passed inline, matching every
/// other `--body-file` caller in this harness (arbitrary suite output as a shell argument is
/// how a stray backtick becomes command substitution).
pub fn file_judgement(env: &Env, repo: &Repo, j: &batcher::core::Judgement, members: &[String], evidence: &str) -> Result<String, String> {
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
    let start = out.find('{').ok_or_else(|| format!("bead.sh file: no JSON in output: {out}"))?;
    let v: serde_json::Value = serde_json::from_str(&out[start..]).map_err(|e| format!("bead.sh file: unparsed output: {e}"))?;
    let v = match v {
        serde_json::Value::Array(a) => a.into_iter().next().ok_or("bead.sh file: empty JSON array")?,
        o => o,
    };
    v.get("id").and_then(|i| i.as_str()).map(str::to_string).ok_or_else(|| format!("bead.sh file: no id in output: {out}"))
}

/// File the mender's bead for a branch that does not merge onto the landing ref; returns its id.
pub fn file_conflict_mend(env: &Env, repo: &Repo, id: &str, tip: &str, files: &[String]) -> Result<String, String> {
    let title = format!("mend {id}: does not merge onto {}", repo.base);
    let body = format!(
        "Bead {id} (branch spira/{id}, tip {tip}) does not merge onto {base} in {repo}.\n\nConflicting files: {files}\n\nRebase the branch onto {base}, resolve each conflict so the bead's intent is kept, run the repo's local checks for what you touched, and resubmit the new tip. If the conflict is not mechanical (the two sides disagree about behaviour), say so in your diagnosis and leave the branch alone: Sift returns the bead to REWORK when this mend's hold runs out.\n",
        base = repo.base,
        repo = repo.name,
        files = files.join(", "),
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("conflict-mend-{id}-{}.txt", now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        Command::new("bead.sh").args(["file", &title, "--for", "mender", "--repo", &repo.name, "--harness-title", "--body-file"]).arg(&tmp).arg("--json"),
        "bead.sh file",
    );
    let _ = fs::remove_file(&tmp);
    bead_id(&out?)
}

/// File the follow-up bead for a deleted flip and return its id.
pub fn file_flip_bead(env: &Env, repo: &Repo, suite: &str, round_branch: &str) -> Result<String, String> {
    let title = format!("{}: {suite} flipped in a batcher round and was deleted — restore it fixed", repo.name);
    let body = format!(
        "{suite} was red in the round's corpus and green on every re-run alone (twice on the round tree, on the base, and on each member touching it), so the batcher deleted it from the round ({round_branch}) rather than send a flip to CI (law-a-test-that-flips-is-deleted).\n\nDeliverable: find the nondeterminism, fix it and re-add the suite.\n"
    );
    let tmp_dir = env.run.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;
    let tmp = tmp_dir.join(format!("flip-{}-{}.txt", repo.name, now()));
    fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let out = run(
        Command::new("bead.sh").args(["file", &title, "--for", "batcher", "--repo", &repo.name, "--harness-title", "--body-file"]).arg(&tmp).arg("--json"),
        "bead.sh file",
    );
    let _ = fs::remove_file(&tmp);
    bead_id(&out?)
}

/// Commit the deletion of `flips` (suite, follow-up bead) into the round worktree, naming
/// the follow-up beads in the message.
pub fn commit_flip_deletion(env: &Env, wt: &Path, flips: &[(String, String)]) -> Result<(), String> {
    let date = run(Command::new("date").arg("+%F"), "date")?;
    crate::flip::apply(wt, flips, date.trim())?;
    run(Command::new("git").arg("-C").arg(wt).args(["add", "-A"]), "git add")?;
    let names: Vec<String> = flips.iter().map(|(s, b)| format!("{s} ({b})")).collect();
    let msg = format!("batcher: delete flipped suite(s) {}", names.join(", "));
    run(
        Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["-c", &format!("user.name={}", env.git_name), "-c", &format!("user.email={}", env.git_email)])
            .args(["commit", "-q", "-m", &msg]),
        "git commit",
    )?;
    Ok(())
}

/// The checks a suite deletion can break: the plan-matrix and its orphan fence, where the
/// round tree has them. Err names what failed.
pub fn recheck_after_deletion(wt: &Path, base_sha: &str) -> Result<(), String> {
    for (script, args) in [("plan-matrix.sh", vec![]), ("plan-lint.sh", vec!["--orphans", base_sha])] {
        let path = wt.join("spira").join(script);
        if !path.exists() {
            continue;
        }
        run(Command::new("bash").arg(&path).args(&args).current_dir(wt), script)?;
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
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
    }
}

fn lock_at(path: &Path, verb: &str) -> Result<Option<Lock>, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let f = fs::OpenOptions::new().create(true).write(true).truncate(false).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return Ok(None);
    }
    let _ = f.set_len(0);
    let _ = (&f).write_all(format!("pid={} verb={verb}\n", std::process::id()).as_bytes());
    Ok(Some(Lock { file: f }))
}

/// None means another operation already holds the lock — not an error, the caller should
/// simply skip this pass, same as batch.sh's own `flock -n` behaviour. The holder is recorded
/// in the lock file so a waiter can name it.
pub fn try_lock(env: &Env, repo: &str) -> Result<Option<Lock>, String> {
    lock_at(&env.queue_dir.join(repo).join("lock"), "batcher")
}

fn check_lock_path(env: &Env, repo: &Repo) -> PathBuf {
    env.run.join("local-check-lock").join(&repo.name)
}

/// One local-tier check at a time per repo: the scheduled checks and a batcher pass both go through it.
pub fn try_check_lock(env: &Env, repo: &Repo) -> Result<Option<Lock>, String> {
    lock_at(&check_lock_path(env, repo), "forge local check")
}

/// A holder that is merely mid-fork elsewhere in this process releases within moments, so a short
/// wait tells that from a check that is really running.
pub fn wait_check_lock(env: &Env, repo: &Repo, wait_secs: u64) -> Result<Option<Lock>, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs);
    loop {
        if let Some(l) = try_check_lock(env, repo)? {
            return Ok(Some(l));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts the repo's local checks detached, unless one is already running. The pass never waits
/// on it: it holds the queue lock, and a tier run takes as long as it takes.
pub fn start_check(env: &Env, repo: &Repo) -> Result<bool, String> {
    if try_check_lock(env, repo)?.is_none() {
        return Ok(false);
    }
    // batch-job: detached tier run lasts as long as the tiers do
    let mut cmd = Command::new(&env.checks_bin);
    cmd.args(["checks-forge", "--repo", &repo.name, "--home"]).arg(&env.home).arg("--run").arg(&env.run);
    if let Some(db) = &env.db {
        cmd.arg("--db").arg(db);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.process_group(0);
    cmd.spawn().map_err(|e| format!("{}: {e}", env.checks_bin.display()))?;
    Ok(true)
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

    fn repo(land: Land) -> Repo {
        Repo { name: "r".into(), path: PathBuf::from("/nonexistent"), base: "local/main".into(), forge: PathBuf::new(), land }
    }

    // POSITIVE CONTROL: under Forge land, push_branch reaches the real git push (and fails on
    // this nonexistent path for an ordinary IO reason) rather than tripping the assert —
    // proving the assert is keyed on `land`, not unconditional.
    #[test]
    fn push_branch_under_forge_land_does_not_assert() {
        let r = repo(Land::Forge);
        let err = push_branch(&r, "deadbeef", "spira/queue/1").unwrap_err();
        assert!(!err.is_empty());
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn fixture(tag: &str) -> (testkit::TempDir, PathBuf, Repo) {
        let root = testkit::TempDir::new(&format!("confirm-base-{tag}"));
        let (remote, work, other) = (root.join("remote.git"), root.join("work"), root.join("other"));
        git(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
        git(&root, &["clone", "-q", remote.to_str().unwrap(), work.to_str().unwrap()]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        git(&root, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);
        let r = Repo { name: "r".into(), path: work.clone(), base: "origin/main".into(), forge: PathBuf::new(), land: Land::Forge };
        (root, other, r)
    }

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
        assert_eq!(foreign_in(&subjects, &members), vec!["sp-lc1puh".to_string()]);
        assert!(foreign_in(&subjects[2..4], &members).is_empty());
    }

    // Round N+1 prepared on batch N's head after N's member was ejected: the ejected
    // member's commits are in base..head and must be named.
    #[test]
    fn foreign_beads_names_an_ejected_members_commits_in_the_head() {
        let (_root, other, r) = fixture("foreign");
        let base = git(&r.path, &["rev-parse", "origin/main"]);
        git(&other, &["pull", "-q", "origin", "main"]);
        git(&other, &["commit", "-q", "--allow-empty", "-m", "spira: land sp-lc1puh"]);
        git(&other, &["commit", "-q", "--allow-empty", "-m", "spira: land sp-94a87q"]);
        git(&other, &["push", "-q", "origin", "HEAD:refs/heads/next"]);
        git(&r.path, &["fetch", "-q", "origin"]);
        let head = git(&r.path, &["rev-parse", "origin/next"]);
        let got = foreign_beads(&r, &base, &head, &["sp-94a87q".to_string()]).unwrap();
        assert_eq!(got, vec!["sp-lc1puh".to_string()]);
        let clean = foreign_beads(&r, &base, &head, &["sp-94a87q".to_string(), "sp-lc1puh".to_string()]).unwrap();
        assert!(clean.is_empty());
    }

    // The remote advances after the round was built: confirm_base must refuse. Without the
    // fetch the stale origin/main would still be an ancestor and the PR would open.
    #[test]
    fn confirm_base_refuses_when_the_remote_base_moved() {
        let (_root, other, r) = fixture("moved");
        let head = git(&r.path, &["rev-parse", "HEAD"]);
        git(&other, &["commit", "-q", "--allow-empty", "-m", "landed meanwhile"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        let err = confirm_base(&r, &head).unwrap_err();
        assert!(err.contains("does not descend"), "{err}");
    }

    // POSITIVE CONTROL: an unmoved base passes, and returns the fetched value.
    #[test]
    fn confirm_base_returns_the_fetched_base_when_head_descends() {
        let (_root, other, r) = fixture("fresh");
        git(&other, &["commit", "-q", "--allow-empty", "-m", "landed before the round"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        git(&r.path, &["pull", "-q", "origin", "main"]);
        git(&r.path, &["commit", "-q", "--allow-empty", "-m", "round"]);
        let head = git(&r.path, &["rev-parse", "HEAD"]);
        let want = git(&other, &["rev-parse", "HEAD"]);
        assert_eq!(confirm_base(&r, &head).unwrap(), want);
    }

    #[test]
    #[should_panic(expected = "unreachable under queue.local")]
    fn push_branch_under_local_land_is_unreachable() {
        let r = repo(Land::Local);
        let _ = push_branch(&r, "deadbeef", "spira/queue/1");
    }

}
#[cfg(test)]
mod forge_default_tests {
    use super::*;

    // The default must be a bare `forge`, and a bare `forge` must be found on the child's PATH
    // alone. PATH goes to the spawn, never into this process: other tests fork concurrently.
    #[test]
    fn a_bare_forge_stub_on_the_childs_path_is_reached() {
        let dir = testkit::TempDir::new("batcher-cut-default-forge-test");
        testkit::write_exe(
            dir.join("forge"),
            "#!/bin/sh
case \"$1\" in pr-create) cat >/dev/null; echo 42 ;; pr-automerge) [ \"$4\" = merge ] && exit 0; exit 1 ;; *) exit 1 ;; esac
",
        );
        let real_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path = dir.path().as_os_str().to_os_string();
        path.push(":");
        path.push(&real_path);

        let repo = Repo { name: "r".into(), path: PathBuf::from("/tmp/r"), base: "local/main".into(), forge: PathBuf::from("forge"), land: Land::Forge };
        let pr = forge_pr_create_on(&repo, "head", "base", "title", "body", Some(&path)).expect("forge_pr_create should reach the stub");
        assert_eq!(pr, "42");
        forge_pr_arm_on(&repo, &pr, Some(&path)).expect("a batch PR is armed with the merge method");
        let bare = Repo { forge: PathBuf::from("forge-missing"), ..repo };
        assert!(forge_pr_arm_on(&bare, &pr, Some(&path)).is_err(), "an arm failure is an error, not a warning");
    }

    fn g(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn member_fixture(d: &Path) -> (Repo, String, Member) {
        g(d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("a"), "a\n").unwrap();
        g(d, &["add", "-A"]);
        g(d, &["commit", "-qm", "base"]);
        let base = g(d, &["rev-parse", "HEAD"]);
        g(d, &["checkout", "-q", "-b", "spira/m1"]);
        fs::write(d.join("m1"), "one\n").unwrap();
        g(d, &["add", "-A"]);
        g(d, &["commit", "-qm", "m1"]);
        let tip = g(d, &["rev-parse", "HEAD"]);
        g(d, &["checkout", "-q", "main"]);
        let r = Repo { name: "r".into(), path: d.to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Forge };
        let m = Member { id: "m1".into(), tip, title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: Vec::new() };
        (r, base, m)
    }

    #[test]
    fn member_with_new_content_since_the_round_is_moved() {
        let d = testkit::TempDir::new("batcher-cut-moved");
        let (r, base, m) = member_fixture(d.path());
        assert!(moved_members(&r, &base, std::slice::from_ref(&m)).is_empty(), "untouched member must not read as moved");
        g(d.path(), &["checkout", "-q", "spira/m1"]);
        fs::write(d.path().join("m1b"), "more\n").unwrap();
        g(d.path(), &["add", "-A"]);
        g(d.path(), &["commit", "-qm", "more"]);
        g(d.path(), &["checkout", "-q", "main"]);
        assert_eq!(moved_members(&r, &base, std::slice::from_ref(&m)), vec!["m1".to_string()]);
    }

    #[test]
    fn member_only_rebased_is_not_moved() {
        let d = testkit::TempDir::new("batcher-cut-rebased");
        let (r, base, m) = member_fixture(d.path());
        fs::write(d.path().join("b"), "b\n").unwrap();
        g(d.path(), &["add", "-A"]);
        g(d.path(), &["commit", "-qm", "main moves"]);
        let newbase = g(d.path(), &["rev-parse", "HEAD"]);
        g(d.path(), &["checkout", "-q", "spira/m1"]);
        g(d.path(), &["rebase", "-q", "main"]);
        g(d.path(), &["checkout", "-q", "main"]);
        assert_ne!(g(d.path(), &["rev-parse", "spira/m1"]), m.tip);
        assert!(moved_members(&r, &newbase, std::slice::from_ref(&m)).is_empty());
        let _ = base;
    }
}

#[cfg(test)]
mod pool_parity_tests {
    use super::*;

    #[test]
    fn tip_current_admits_the_live_tip_and_excludes_a_moved_one() {
        let mut branches = BTreeMap::new();
        branches.insert("sp-moved".to_string(), "newtip".to_string());
        branches.insert("sp-stable".to_string(), "sametip".to_string());

        assert!(!tip_current("oldtip", "sp-moved", &branches), "a certified tip that the branch has moved past must not be admitted");
        // POSITIVE CONTROL: the same branch's own live tip is admitted — proves the check
        // discriminates on the tip, not on the id being present at all.
        assert!(tip_current("newtip", "sp-moved", &branches));
        assert!(tip_current("sametip", "sp-stable", &branches));
        assert!(!tip_current("sametip", "sp-unknown", &branches), "no branch at all is not current");
    }

    fn git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success(), "git {args:?}");
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        run(Command::new("git").arg("-C").arg(dir).args(args), "git").unwrap().trim().to_string()
    }

    fn tmp_repo(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(&format!("batcher-cut-pool-{tag}"));
        git(&d, &["init", "-q", "-b", "main"]);
        git(&d, &["config", "user.email", "t@t"]);
        git(&d, &["config", "user.name", "t"]);
        fs::write(d.join("base.txt"), "base").unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", "base"]);
        d
    }

    /// Branches a spira/<name> ref off main and commits `content` to it, returning the tip.
    fn branch(dir: &Path, name: &str, content: &str) -> String {
        git(dir, &["checkout", "-q", "-B", &format!("spira/{name}"), "main"]);
        fs::write(dir.join(format!("{name}.txt")), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", name]);
        git_out(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn repo_branch_tips_reads_the_live_tip_not_a_remembered_one() {
        let d = tmp_repo("tips");
        let tip1 = branch(&d, "sp-test1", "v1");
        // THE MOVE: a further commit lands on the branch, un-gated — exactly the shape a
        // batch.sh re-certification races against (law-batcher-earns-the-round-by-parity).
        fs::write(d.join("sp-test1-more.txt"), "v2").unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", "moved past certification"]);
        let moved_tip = git_out(&d, &["rev-parse", "spira/sp-test1"]);
        assert_ne!(tip1, moved_tip, "positive control: the branch really did move");

        let repo = Repo { name: "r".into(), path: d.path().to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Forge };
        let branches = repo_branch_tips(&repo).unwrap();

        assert!(!tip_current(&tip1, "sp-test1", &branches), "the OLD certified tip is no longer this branch's live tip");
        assert!(tip_current(&moved_tip, "sp-test1", &branches), "the branch's actual live tip is current");

        let _ = fs::remove_dir_all(&d);
    }
    /// A pool fixture: a repo with `spira/<id>` branches, a spira-lc stand-in answering
    /// `list` (the given certified ids at their live tips, then every
    /// row in `lc_rows`) and `show`, and a bd stand-in whose `list` honours `--status` the way
    /// bd does — so a reader that still filtered on bd status would see bd's answer.
    /// The `content-rows` case arm of a `spira-lc` stub, answering from the fixture's `show-*` and `list-all` files.
    fn content_rows_arm(sd: &str) -> String {
        format!("content-rows) echo content-rows >> {sd}/content.log; if [ \"$2\" = --external-contains ]; then cat {sd}/list-all; else IFS=, read -ra a <<<\"$4\"; o=; for i in \"${{a[@]}}\"; do [ -f {sd}/show-$i ] && o=\"$o${{o:+,}}$(cat {sd}/show-$i)\"; done; echo \"[$o]\"; fi ;;")
    }

    fn pool_fixture(tag: &str, certified: &[&str], lc_rows: &[(&str, &str)], bd_rows: &[(&str, &str, &str, &str)]) -> (testkit::TempDir, testkit::TempDir, Env, Repo) {
        let d = tmp_repo(tag);
        let stubs = testkit::TempDir::new(&format!("batcher-cut-pool-stubs-{tag}"));
        let mut cert = Vec::new();
        for id in certified {
            let tip = branch(&d, id, id);
            cert.push(format!(r#"{{"bead_id":"{id}","state":"CERTIFIED","tip":"{tip}","updated_at":5}}"#));
        }
        let all: Vec<String> = lc_rows.iter().map(|(id, st)| format!(r#"{{"bead_id":"{id}","state":"{st}","holds":[]}}"#)).collect();
        let every_row: Vec<String> = cert.iter().cloned().chain(all).collect();
        fs::write(stubs.join("lc-all"), format!("[{}]", every_row.join(","))).unwrap();
        let row = |(id, status, ext, body): &(&str, &str, &str, &str)| {
            format!(r#"{{"id":"{id}","status":"{status}","title":"t {id}","priority":2,"labels":["repo:r"],"external_ref":"{ext}","description":"{body}"}}"#)
        };
        for r in bd_rows {
            fs::write(stubs.join(format!("show-{}", r.0)), row(r)).unwrap();
        }
        // `list` is only ever `--external-contains basefail:…`: the base-red rows.
        let every: Vec<String> = bd_rows.iter().filter(|r| !r.2.is_empty()).map(row).collect();
        fs::write(stubs.join("list-all"), format!("[{}]", every.join(","))).unwrap();
        let sd = stubs.path().display().to_string();
        testkit::write_exe(
            stubs.join("spira-lc"),
            &format!("#!/bin/bash\ncase \"$1\" in list) cat {sd}/lc-all ;; show*) echo '{{\"bead\":{{\"bead_id\":\"'$2'\"}}}}' ;; {} esac\n", content_rows_arm(&sd)),
        );
        testkit::write_exe(stubs.join("bd"), &format!("#!/bin/bash\ntouch {sd}/bd-spawned\nexit 99\n"));
        let mut env = super::lifecycle_tests_env(stubs.path());
        env.lc_bin = Some(stubs.join("spira-lc"));
        env.bd = stubs.join("bd").display().to_string();
        let repo = Repo { name: "r".into(), path: d.path().to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Forge };
        (d, stubs, env, repo)
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
            stack: prereq.map(|p| (p.to_string(), format!("{p}-tip"))).into_iter().collect(),
            blocked_by: vec![],
        };
        let pool = || vec![member("sp-q", Some("sp-p")), member("sp-r", Some("sp-q")), member("sp-s", None)];
        let held: std::collections::BTreeSet<String> = ["sp-p".to_string()].into();
        let kept: Vec<String> = drop_stacked_on_barred(pool(), held).into_iter().map(|m| m.id).collect();
        assert_eq!(kept, ["sp-s"]);
        let lifted = drop_stacked_on_barred(pool(), Default::default());
        assert_eq!(lifted.len(), 3);
    }

    /// An idle tick has no certified bead, and asks bd nothing: no member can be a base fix.
    #[test]
    fn an_empty_pool_asks_bd_for_nothing() {
        let (_d, stubs, env, repo) = pool_fixture("lc-empty", &[], &[], &[("sp-b1", "open", "basefail:r:x", "")]);
        assert!(certified_pool(&env, &repo).unwrap().is_empty());
        assert!(!stubs.join("content.log").exists(), "the content store was read for an empty pool");
    }

    /// The sift path reads only the beads it screens: a store whose full list would be over 64 KiB
    /// is never listed whole, and a stacked prerequisite outside the pool is read by id.
    #[test]
    fn the_pool_is_read_without_listing_the_whole_store() {
        let (_d, stubs, env, repo) = pool_fixture("lc-scoped", &["sp-c1"], &[("sp-c1", "CERTIFIED")], &[("sp-c1", "open", "", "")]);
        let sd = stubs.path().display().to_string();
        testkit::write_exe(
            stubs.join("spira-lc"),
            &format!(
                "#!/bin/bash\necho \"$@\" >> {sd}/lc.log\nfor a in \"$@\"; do case \"$a\" in --state|--ids) f=1;; esac; done\ncase \"$1\" in list) if [ -z \"$f\" ]; then head -c 200000 /dev/zero | tr '\\0' x; exit 0; fi; cat {sd}/lc-all ;; show*) echo '{{\"bead\":{{\"bead_id\":\"'$2'\"}}}}' ;; {} esac\n",
                content_rows_arm(&sd)
            ),
        );
        let ids: Vec<String> = certified_pool(&env, &repo).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["sp-c1"]);
        let log = fs::read_to_string(stubs.join("lc.log")).unwrap();
        assert!(log.lines().filter(|l| l.starts_with("list")).all(|l| l.contains("--state") || l.contains("--ids")), "{log}");
    }

    /// A sift pass over N beads (pool, base-fix lookup, per-bead description read) spawns no bd.
    #[test]
    fn a_sift_pass_spawns_no_bd() {
        let ids = ["sp-c1", "sp-c2", "sp-c3", "sp-c4"];
        let lc: Vec<(&str, &str)> = ids.iter().map(|i| (*i, "CERTIFIED")).chain([("sp-bf1", "READY")]).collect();
        let bd: Vec<(&str, &str, &str, &str)> = ids.iter().map(|i| (*i, "open", "", "fixes sp-bf1")).chain([("sp-bf1", "open", "basefail:r:t.sh", "")]).collect();
        let (_d, stubs, env, repo) = pool_fixture("lc-nobd", &ids, &lc, &bd);
        let pool = certified_pool(&env, &repo).unwrap();
        assert_eq!(pool.len(), ids.len());
        assert!(pool.iter().all(|m| m.base_fix), "the base-fix lookup ran through the content reader");
        for id in ids {
            bd_show_text(&env, id).unwrap();
        }
        assert!(!stubs.join("bd-spawned").exists(), "a bd process was spawned");
    }

    /// sp-mve9i: the pool is the lifecycle machine's CERTIFIED rows. bd's status — here a
    /// bd row someone closed by hand, carrying no submitted label — decides nothing.
    #[test]
    fn a_certified_bead_is_a_member_whatever_bd_says_its_status_is() {
        let (_d, _s, env, repo) = pool_fixture("lc-member", &["sp-c1"], &[("sp-c1", "CERTIFIED")], &[("sp-c1", "closed", "", "")]);
        let ids: Vec<String> = certified_pool(&env, &repo).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["sp-c1"]);
    }

    /// A blocks edge present in the content store but absent from the lifecycle row still keeps
    /// the member out of a cut, naming the blocker; a landed prerequisite does not.
    #[test]
    fn a_content_store_blocks_edge_the_lifecycle_mirror_lacks_blocks_the_member() {
        let (_d, stubs, env, repo) = pool_fixture("lc-edge", &["sp-a1", "sp-b1"], &[("sp-a1", "CERTIFIED"), ("sp-b1", "CERTIFIED")], &[("sp-a1", "open", "", ""), ("sp-b1", "open", "", "")]);
        let edged = |dep: &str| {
            format!(r#"{{"id":"sp-a1","title":"t","priority":2,"labels":[],"external_ref":"","description":"","dependencies":[{{"depends_on_id":"{dep}","type":"blocks"}},{{"depends_on_id":"sp-epic","type":"parent-child"}}]}}"#)
        };
        fs::write(stubs.join("show-sp-a1"), edged("sp-b1")).unwrap();
        let blocked_of = |env: &Env| certified_pool(env, &repo).unwrap().into_iter().find(|m| m.id == "sp-a1").unwrap().blocked_by;
        assert_eq!(blocked_of(&env), ["sp-b1"], "only the blocks edge counts, not parent-child");

        fs::write(stubs.join("show-sp-a1"), edged("sp-done")).unwrap();
        let sd = stubs.path().display().to_string();
        testkit::write_exe(
            stubs.join("spira-lc"),
            &format!(
                "#!/bin/bash\ncase \"$1\" in list) cat {sd}/lc-all ;; show*) if [ \"$2\" = sp-done ]; then echo '{{\"bead\":{{\"state\":\"LANDED\"}}}}'; else echo '{{\"bead\":{{\"state\":\"CERTIFIED\"}}}}'; fi ;; {} esac\n",
                content_rows_arm(&sd)
            ),
        );
        assert!(blocked_of(&env).is_empty(), "a landed prerequisite blocks nothing");
    }

    /// sp-mve9i: a base-red bead is open while its lifecycle row is not terminal — a bd row
    /// closed by hand on a READY base-red still accelerates its fix, and a bd row left open
    /// on a LANDED one no longer does.
    #[test]
    fn a_base_red_bead_is_open_by_its_lifecycle_row_not_bd_status() {
        let (_d, _s, env, repo) = pool_fixture(
            "lc-basefix",
            &["sp-c1", "sp-c2"],
            &[("sp-c1", "CERTIFIED"), ("sp-c2", "CERTIFIED"), ("sp-bf1", "READY"), ("sp-bf2", "LANDED")],
            &[
                ("sp-c1", "open", "", "fixes sp-bf1"),
                ("sp-c2", "open", "", "fixes sp-bf2"),
                ("sp-bf1", "closed", "basefail:r:test-a.sh", ""),
                ("sp-bf2", "open", "basefail:r:test-b.sh", ""),
            ],
        );
        let fixes: Vec<String> = certified_pool(&env, &repo).unwrap().into_iter().filter(|m| m.base_fix).map(|m| m.id).collect();
        assert_eq!(fixes, vec!["sp-c1"]);
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
        assert_eq!(result_status(&d, "test-q.sh"), Some(true), "quarantined never blocks");
        fs::write(d.join("test-t.sh.result"), "timeout 1 2 fp p e 124").unwrap();
        assert_eq!(result_status(&d, "test-t.sh"), Some(false));
        fs::write(d.join("test-h.sh.result"), "").unwrap();
        assert_eq!(result_status(&d, "test-h.sh"), None, "half-written: not final yet");
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
mod lifecycle_tests {
    //! The lifecycle machine (DESIGN.md §3): spira-lc runs, and an unreachable machine
    //! fails the probe.
    use super::*;

    fn scratch(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("batcher-cut-lc-{tag}"))
    }

    /// An executable spira-lc stand-in that logs its argv and answers `reply`.
    fn fake_lc(dir: &Path, reply: &str) -> PathBuf {
        let p = dir.join("spira-lc");
        testkit::write_exe(&p, &format!("#!/bin/sh\necho \"$@\" >> '{}'\nprintf '%s' '{reply}'\n", dir.join("lc.log").display()));
        p
    }

    fn env(dir: &Path, lc_bin: Option<PathBuf>) -> Env {
        Env { lc_bin, ..super::lifecycle_tests_env(dir) }
    }

    fn members() -> Vec<(String, String)> {
        vec![("sp-a".into(), "aaaa".into()), ("sp-b".into(), "bbbb".into())]
    }

    #[test]
    fn a_cut_that_recorded_no_batch_id_writes_neither_key() {
        let d = scratch("no-batch-id");
        let e = env(&d, None);
        let ob = OpenBatch { pr: "7".into(), members: members(), owner: "batcher".into(), ..OpenBatch::default() };
        write_open_batch(&e, "r", &ob).unwrap();
        let text = fs::read_to_string(d.join("queue/r/open")).unwrap();
        assert!(!text.contains("batch_id=") && !text.contains("version="), "{text}");
    }

    #[test]
    fn the_round_pool_is_the_machines_unheld_submitted_and_certified_beads() {
        let d = scratch("certified");
        let reply = r#"[{"bead_id":"sp-b","state":"CERTIFIED","tip":"bbbb","updated_at":1790000002,"express":"1"},{"bead_id":"sp-a","state":"SUBMITTED","tip":"aaaa","updated_at":"1790000001"},{"bead_id":"sp-n","state":"CERTIFIED","tip":null,"updated_at":null},{"bead_id":"sp-x","state":"OPEN","tip":"xxxx"}]"#;
        let e = env(&d, Some(fake_lc(&d, reply)));
        assert_eq!(
            read_pool(&e).unwrap(),
            vec![("sp-a".to_string(), "aaaa".to_string(), 1790000001, false, vec![]), ("sp-b".into(), "bbbb".into(), 1790000002, true, vec![]), ("sp-n".into(), "none".into(), 0, false, vec![])]
        );
        let log = fs::read_to_string(d.join("lc.log")).unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), vec!["list --state SUBMITTED,CERTIFIED"], "{log}");
        assert!(read_pool(&env(&d, Some(fake_lc(&d, "not json")))).unwrap_err().contains("unparsed"));
        assert!(read_pool(&env(&d, None)).is_err());
    }

    #[test]
    fn a_tip_a_round_ejected_red_is_not_in_the_round_pool_but_a_new_one_is() {
        let d = scratch("ejected-red");
        let reply = r#"[{"bead_id":"sp-r","state":"CERTIFIED","tip":"rrrr","ejected_red_tip":"rrrr","updated_at":4},{"bead_id":"sp-n","state":"CERTIFIED","tip":"nnnn","ejected_red_tip":"oooo","updated_at":5},{"bead_id":"sp-c","state":"CERTIFIED","tip":"cccc","ejected_red_tip":null,"updated_at":6}]"#;
        let e = env(&d, Some(fake_lc(&d, reply)));
        let ids: Vec<String> = read_pool(&e).unwrap().into_iter().map(|r| r.0).collect();
        assert_eq!(ids, vec!["sp-c".to_string(), "sp-n".to_string()]);
    }

    #[test]
    fn a_held_row_is_not_in_the_round_pool() {
        let d = scratch("held");
        let reply = r#"[{"bead_id":"sp-h","state":"CERTIFIED","tip":"hhhh","since":5,"updated_at":9,"holds":["ask"]},{"bead_id":"sp-s","state":"CERTIFIED","tip":"ssss","since":3,"updated_at":9,"holds":"[]"},{"bead_id":"sp-j","state":"CERTIFIED","tip":"jjjj","updated_at":4,"holds":[]},{"bead_id":"sp-t","state":"CERTIFIED","tip":"tttt","updated_at":4,"holds":"[\"wait\"]"}]"#;
        let e = env(&d, Some(fake_lc(&d, reply)));
        assert_eq!(
            read_pool(&e).unwrap(),
            vec![("sp-j".to_string(), "jjjj".to_string(), 4, false, vec![]), ("sp-s".into(), "ssss".into(), 3, false, vec![])]
        );
    }

    #[test]
    fn runs_spira_lc_and_records_the_batch() {
        let d = scratch("cut");
        let e = env(&d, Some(fake_lc(&d, r#"[]"#)));
        assert_eq!(lc_probe(&e), Ok(()));
        assert_eq!(lc_cut_batch(&e, "r", "spira/queue/1", "h", "b", &members()), Some("2".into()));
        let log = fs::read_to_string(d.join("lc.log")).unwrap();
        assert!(log.starts_with("list --state SUBMITTED,CERTIFIED\n"), "{log}");
        assert!(log.contains("cut spira/queue/1 --repo r --head h --base b --members sp-a:aaaa,sp-b:bbbb"), "{log}");
    }

    #[test]
    fn reads_the_stack_off_the_machine() {
        let d = scratch("stack");
        let e = env(&d, Some(fake_lc(&d, r#"{"bead":{"stack":{"sp-z":"zzzz"}}}"#)));
        assert_eq!(read_stack(&e, "sp-a").get("sp-z").map(String::as_str), Some("zzzz"));
    }

    #[test]
    fn a_landed_prerequisite_drops_out_of_the_stack_a_live_one_stays() {
        let d = scratch("stack-landed");
        let p = d.join("spira-lc");
        testkit::write_exe(
            &p,
            "#!/bin/sh\ncase \"$2\" in\n sp-dep) printf '%s' '{\"bead\":{\"stack\":{\"sp-done\":\"t1\",\"sp-live\":\"t2\",\"sp-gone\":\"t3\"}}}';;\n sp-done) printf '%s' '{\"bead\":{\"state\":\"LANDED\"}}';;\n sp-gone) printf '%s' '{\"bead\":{\"state\":\"DROPPED\"}}';;\n *) printf '%s' '{\"bead\":{\"state\":\"CERTIFIED\"}}';;\nesac\n",
        );
        let e = env(&d, Some(p));
        let stack = read_stack(&e, "sp-dep");
        assert_eq!(stack.keys().map(String::as_str).collect::<Vec<_>>(), ["sp-live"]);
    }

    #[test]
    fn an_unreachable_machine_fails_the_probe() {
        let d = scratch("gone");
        let e = env(&d, Some(d.join("nonexistent-spira-lc")));
        assert!(lc_probe(&e).is_err());
        let e = env(&d, None);
        assert_eq!(lc_probe(&e), Err("no spira-lc program".to_string()));
        // A reachable binary with a non-JSON reply is not an answer either.
        let e = env(&d, Some(fake_lc(&d, "not json")));
        assert!(lc_probe(&e).unwrap_err().contains("unparsed"));
    }
}

#[cfg(test)]
mod certify_tests {
    //! The round certificate (DESIGN.md "Round certificate"; queue/DESIGN.md §8 D12).
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn reaping_an_aborted_round_leaves_no_attr_branch_and_the_stamp_is_utc() {
        let d = testkit::TempDir::new("batcher-cut-reap");
        git(&d, &["init", "-q"]);
        fs::write(d.join("a"), "1").unwrap();
        git(&d, &["add", "a"]);
        git(&d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a"]);
        let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "local/main".into(), forge: PathBuf::new(), land: Land::Local };
        for b in ["spira/batcher-attr/spira-1-j1", "spira/batcher-attr/spira-1-survivors", "spira/batcher-attr/other-1"] {
            set_branch(&repo, b, "HEAD");
        }
        assert_eq!(local_branches(&repo, "spira/batcher-attr/spira-*").len(), 2);
        reap_branches(&repo, "spira/batcher-attr/spira-*");
        assert_eq!(local_branches(&repo, "spira/batcher-attr/*"), vec!["spira/batcher-attr/other-1".to_string()]);
        assert!(local_branches(&repo, "spira/queue/*").is_empty());
        assert_eq!(utc_stamp(1790551299), "20260927T232139Z");
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(951782400), "20000229T000000Z");
        let _ = fs::remove_dir_all(&d);
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
    fn the_first_fail_line_is_found_and_absence_is_none() {
        assert_eq!(first_fail_line("ok 1\n  FAIL widget broke\nFAIL later\n").as_deref(), Some("FAIL widget broke"));
        assert_eq!(first_fail_line("not ok 3 - x\n").as_deref(), Some("not ok 3 - x"));
        assert_eq!(first_fail_line("all fine\nPASS\n"), None);
        let d = testkit::TempDir::new("batcher-cut-failline");
        fs::write(d.join("test-a.sh.out"), "setup\nFAIL: boom\n").unwrap();
        assert_eq!(suite_first_fail(&d, "test-a.sh").as_deref(), Some("FAIL: boom"));
        assert_eq!(suite_first_fail(&d, "test-missing.sh"), None);
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod eject_tests {
    use super::*;

    #[test]
    fn an_ejected_member_carries_its_suites_to_the_sidecar_and_the_row() {
        let d = testkit::TempDir::new("batcher-cut-eject");
        // A lib.sh that records each call's argv, one call per line, fields tab-separated.
        let log = d.join("calls");
        let rec = |f: &str| format!("{f}() {{ (IFS=$'\\t'; printf '{f}\\t%s\\n' \"$*\") >> '{}'; }}\n", log.display());
        fs::write(d.join("lib.sh"), rec("bead_reopen")).unwrap();
        let e = super::lifecycle_tests_env(&d);
        eject_member(&e, "spira", "sp-m2", &["test-a.sh".into(), "test-b.sh".into()], &[("test-a.sh".into(), "FAIL widget".into())], true);
        let calls = fs::read_to_string(&log).unwrap();
        let lines: Vec<Vec<&str>> = calls.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(lines.len(), 1, "{calls}");
        assert_eq!(lines[0][..3], ["bead_reopen", "sp-m2", "queue-eject-local"]);
        assert_eq!(lines[0][4], "test-a.sh,test-b.sh", "the fourth argument writes <id>.ejected");
        assert!(lines[0][3].contains("test-a.sh: FAIL widget"), "the reason names the suite's first FAIL line: {}", lines[0][3]);
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod lc_withdraw_tests {
    use super::*;

    /// A spira-lc stand-in whose `show` answers the given state at version 4 and whose
    /// `event` calls are logged, one argv per line.
    fn lc_stub(d: &Path, state: &str) -> PathBuf {
        let log = d.join("lc-calls");
        let bin = d.join("spira-lc");
        testkit::write_exe(
            &bin,
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$1\" in show) echo '{{\"bead\":{{\"bead_id\":\"'$2'\",\"state\":\"{state}\",\"version\":4}}}}' ;; esac\nexit 0\n",
                log.display()
            ),
        );
        bin
    }

    /// sp-mve9i: membership is the machine's CERTIFIED state alone, so a member the batcher
    /// withdraws (reopened for rebase after consecutive base conflicts, or ejected by local
    /// attribution) must leave CERTIFIED on the machine too — bd's reopen no longer keeps it
    /// out of the next round. The machine's route is queue eject's: Deliver, then
    /// Returned(batch-ejected), to REWORK.
    #[test]
    fn a_withdrawn_or_ejected_member_is_returned_to_rework_on_the_machine() {
        for (tag, eject) in [("conflict", false), ("eject", true)] {
            let d = testkit::TempDir::new(&format!("batcher-cut-lcw-{tag}"));
            fs::write(d.join("lib.sh"), "bead_reopen() { :; }\n").unwrap();
            let mut e = super::lifecycle_tests_env(&d);
            e.lc_bin = Some(lc_stub(&d, "CERTIFIED"));
            if eject {
                eject_member(&e, "spira", "sp-a", &["test-a.sh".into()], &[], true);
            } else {
                let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "local/main".into(), forge: PathBuf::new(), land: Land::Local };
                withdraw_for_conflict(&e, &repo, "sp-a", 2, &["x.rs".into()]);
            }
            let calls = fs::read_to_string(d.join("lc-calls")).unwrap_or_default();
            assert!(
                calls.contains("event bead sp-a --expect CERTIFIED --version 4 --actor batcher --kind \"Deliver\""),
                "{tag}: {calls}"
            );
            assert!(
                calls.contains("event bead sp-a --expect IN_DELIVERY --version 5 --actor batcher --kind {\"Returned\":{\"reason\":\"batch-ejected\"}}"),
                "{tag}: {calls}"
            );
        }
    }

    /// A SUBMITTED member never got a certificate, so its ejection is the machine's own
    /// GateRed to REWORK, quoted against the tip.
    #[test]
    fn an_ejected_submitted_member_is_gate_red_to_rework() {
        let d = testkit::TempDir::new("batcher-cut-lcw-submitted");
        fs::write(d.join("lib.sh"), "bead_reopen() { :; }\n").unwrap();
        let mut e = super::lifecycle_tests_env(&d);
        e.lc_bin = Some(lc_stub(&d, "SUBMITTED"));
        eject_member(&e, "spira", "sp-a", &["test-a.sh".into()], &[], true);
        let calls = fs::read_to_string(d.join("lc-calls")).unwrap_or_default();
        assert!(calls.contains("event bead sp-a --expect SUBMITTED --version 4 --actor batcher --kind {\"GateRed\":{"), "{calls}");
        assert!(calls.contains("\"reason\":\"suites-failed\""), "{calls}");
        assert!(!calls.contains("Deliver"), "{calls}");
    }

    /// The round's green corpus certifies its SUBMITTED survivors, and only those.
    #[test]
    fn a_green_round_certifies_its_submitted_members_only() {
        for (state, certified) in [("SUBMITTED", true), ("CERTIFIED", false)] {
            let d = testkit::TempDir::new(&format!("batcher-cut-lcc-{state}"));
            let mut e = super::lifecycle_tests_env(&d);
            e.lc_bin = Some(lc_stub(&d, state));
            lc_certify_round(&e, &[("sp-a".into(), "aaaa".into())], "k1");
            let calls = fs::read_to_string(d.join("lc-calls")).unwrap_or_default();
            let passed = calls.contains("\"GatePass\"") && calls.contains("\"gate_key\":\"round:k1\"") && calls.contains("\"tip\":\"aaaa\"");
            assert_eq!(passed, certified, "{state}: {calls}");
        }
    }

    /// A red gate verdict recorded on an IN_DELIVERY member at the tip the round holds names it
    /// for eject; a moved tip, an unmarked row or another state does not.
    #[test]
    fn only_a_member_marked_red_at_the_round_tip_is_named_for_eject() {
        for (state, reason, tip, named) in [
            ("IN_DELIVERY", "gate-red: suites-failed", "aaaa", true),
            ("IN_DELIVERY", "gate-red: suites-failed", "moved", false),
            ("IN_DELIVERY", "delivered-from-submitted", "aaaa", false),
            ("REWORK", "gate-red: suites-failed", "aaaa", false),
        ] {
            let d = testkit::TempDir::new("batcher-cut-gate-red");
            let mut e = super::lifecycle_tests_env(&d);
            let bin = d.join("spira-lc");
            testkit::write_exe(
                &bin,
                &format!("#!/bin/sh\necho '{{\"bead\":{{\"state\":\"{state}\",\"tip\":\"{tip}\",\"reason\":\"{reason}\",\"version\":4}}}}'\n"),
            );
            e.lc_bin = Some(bin);
            let got = gate_red_marks(&e, &[("sp-a".into(), "aaaa".into())]);
            assert_eq!(got, if named { vec![("sp-a".to_string(), "suites-failed".to_string())] } else { vec![] }, "{state} {reason} {tip}");
        }
    }

    /// A member the machine no longer holds CERTIFIED (resubmitted since, or already
    /// returned) is left alone: nothing to withdraw.
    #[test]
    fn a_member_not_certified_on_the_machine_is_not_touched() {
        let d = testkit::TempDir::new("batcher-cut-lcw-rework");
        fs::write(d.join("lib.sh"), "bead_reopen() { :; }\n").unwrap();
        let mut e = super::lifecycle_tests_env(&d);
        e.lc_bin = Some(lc_stub(&d, "REWORK"));
        eject_member(&e, "spira", "sp-a", &[], &[], true);
        let calls = fs::read_to_string(d.join("lc-calls")).unwrap_or_default();
        assert!(!calls.contains("event "), "{calls}");
    }
}

#[cfg(test)]
mod conflict_streak_tests {
    use super::*;

    #[test]
    fn the_second_consecutive_set_aside_withdraws_the_member() {
        let d = testkit::TempDir::new("batcher-cut-streak");
        let log = d.join("calls");
        fs::write(d.join("lib.sh"), format!("bead_reopen() {{ (IFS=$'\\t'; printf 'bead_reopen\\t%s\\n' \"$*\") >> '{}'; }}\n", log.display())).unwrap();
        let e = super::lifecycle_tests_env(&d);
        let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "local/main".into(), forge: PathBuf::new(), land: Land::Local };

        assert_eq!(conflict_streak_bump(&e, "sp-a", "t1"), 1, "the first cut only sets it aside");
        assert_eq!(conflict_streak_bump(&e, "sp-a", "t1"), 2);
        withdraw_for_conflict(&e, &repo, "sp-a", 2, &["x.rs".into(), "y.sh".into()]);
        let calls = fs::read_to_string(&log).unwrap();
        let lines: Vec<Vec<&str>> = calls.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(lines.len(), 1, "{calls}");
        assert_eq!(lines[0][..3], ["bead_reopen", "sp-a", "rebase-conflict"]);
        assert!(lines[0][3].contains("2 consecutive rounds") && lines[0][3].contains("x.rs, y.sh"), "{calls}");
        assert_eq!(conflict_streak_bump(&e, "sp-a", "t1"), 1, "withdrawal clears the count");
        assert_eq!(conflict_streak_bump(&e, "sp-a", "t2"), 1, "a new tip starts over");
        conflict_streak_clear(&e, "sp-a");
        assert_eq!(conflict_streak_bump(&e, "sp-a", "t2"), 1, "a round without the conflict starts over");
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
        assert_eq!(classify_land_exit(None), LandOutcome::Refused, "killed by a signal: nothing is known to have landed");
    }

    fn stub_queue(d: &Path, busy_first: u32, then_exit: i32, then_msg: &str) -> Env {
        let mut e = lifecycle_tests_env(d);
        let n = d.join("n");
        testkit::write_exe(
            &d.join("queue"),
            &format!(
                "#!/bin/sh\ncat >/dev/null\nc=$(cat '{n}' 2>/dev/null || echo 0)\nc=$((c+1)); echo $c > '{n}'\nif [ $c -le {busy_first} ]; then echo 'queue.sh round land: another queue operation holds the lock for spira' >&2; exit 1; fi\n[ -n '{then_msg}' ] && echo '{then_msg}' >&2\nexit {then_exit}\n",
                n = n.display()
            ),
        );
        e.queue_bin = d.join("queue");
        e
    }

    fn repo_at(d: &Path) -> Repo {
        Repo { name: "spira".into(), path: d.to_path_buf(), base: "local/main".into(), forge: d.to_path_buf(), land: Land::Local }
    }

    fn echo_queue(d: &Path, script: &str) -> Env {
        let mut e = lifecycle_tests_env(d);
        testkit::write_exe(&d.join("queue"), &format!("#!/bin/sh\n{script}\n"));
        e.queue_bin = d.join("queue");
        e
    }

    #[test]
    fn a_promotion_names_its_round_head_members_and_whether_it_was_attested() {
        let p = parse_promotion("batch=r2\nhead=abc\nmembers=sp-a:t1 sp-b:t2\nattested=1\n").unwrap();
        assert_eq!(p, Promotion { batch: "r2".into(), head: "abc".into(), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], attested: true });
        assert!(!parse_promotion("batch=r2\nhead=abc\nmembers=sp-a:t1\nattested=0\n").unwrap().attested);
        assert_eq!(parse_promotion("batch=r2\nmembers=sp-a:t1\n"), None, "a promotion with no head names no tree to land");
    }

    #[test]
    fn a_stage_runs_beside_the_caller_and_names_its_batch_only_on_success() {
        let d = testkit::TempDir::new("batcher-cut-stage");
        let e = echo_queue(&d, "cat >/dev/null; echo \"$@\" > '/dev/null'; echo batch=r2; echo not-this >&2");
        let m = Member { id: "sp-a".into(), tip: "t1".into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: Vec::new() };
        let bg = round_stage_spawn(&e, &repo_at(&d), &d.join("wt"), &[m.clone()]).unwrap();
        assert_eq!(round_stage_done(&repo_at(&d), bg), Some("r2".into()));
        let e = echo_queue(&d, "cat >/dev/null; echo batch=r2; exit 1");
        let bg = round_stage_spawn(&e, &repo_at(&d), &d.join("wt"), &[m]).unwrap();
        assert_eq!(round_stage_done(&repo_at(&d), bg), None, "a refused stage staged nothing");
    }

    #[test]
    fn a_stage_test_is_green_only_on_exit_zero_and_a_refused_promote_is_none() {
        let d = testkit::TempDir::new("batcher-cut-stage-test");
        let e = echo_queue(&d, "exit 0");
        assert!(round_stage_test_done(&repo_at(&d), round_stage_test_spawn(&e, &repo_at(&d)).unwrap()));
        let e = echo_queue(&d, "exit 1");
        assert!(!round_stage_test_done(&repo_at(&d), round_stage_test_spawn(&e, &repo_at(&d)).unwrap()));
        assert_eq!(round_promote(&e, &repo_at(&d)), None);
        let e = echo_queue(&d, "echo batch=r2; echo head=h; echo members=sp-a:t1; echo attested=0");
        assert_eq!(round_promote(&e, &repo_at(&d)).map(|p| (p.batch, p.attested)), Some(("r2".into(), false)));
    }

    #[test]
    fn a_held_lock_retries_then_lands() {
        let d = testkit::TempDir::new("batcher-cut-land-retry");
        let e = stub_queue(&d, 2, 0, "");
        let r = round_land(&e, &repo_at(&d), "b1").unwrap();
        assert_eq!(r.outcome, LandOutcome::Landed);
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "3");
    }

    #[test]
    fn a_lock_held_past_the_bound_is_a_refusal_naming_the_line() {
        let d = testkit::TempDir::new("batcher-cut-land-held");
        let e = stub_queue(&d, 99, 0, "");
        let r = round_land(&e, &repo_at(&d), "b1").unwrap();
        assert_eq!(r.outcome, LandOutcome::Refused);
        assert!(r.refusal.contains("holds the lock"), "{}", r.refusal);
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "3", "bounded by land_lock_attempts");
    }

    #[test]
    fn an_eject_on_a_held_lock_retries_and_a_real_refusal_does_not() {
        let d = testkit::TempDir::new("batcher-cut-eject-retry");
        let e = stub_queue(&d, 2, 0, "");
        round_eject(&e, &repo_at(&d), "b1", "sp-a", &[], &[], false).unwrap();
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "3");
        let d = testkit::TempDir::new("batcher-cut-eject-refused");
        let e = stub_queue(&d, 0, 1, "queue.sh round eject: sp-a is not a member");
        let err = round_eject(&e, &repo_at(&d), "b1", "sp-a", &[], &[], false).unwrap_err();
        assert!(err.contains("not a member"), "{err}");
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "1", "no retry");
    }

    #[test]
    fn a_landing_clears_a_loop_pause_but_never_a_deliberate_hold() {
        let d = testkit::TempDir::new("batcher-cut-unpause");
        let pause = d.join("cut.pause");
        let mut e = lifecycle_tests_env(&d);
        assert_eq!(unpause_after_land(&e).unwrap(), Unpause::Off, "default off");
        fs::write(&pause, "ROUND RED").unwrap();
        assert!(pause.exists(), "positive control: the pause exists before the call");
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
    fn a_refusal_without_the_lock_is_final_and_alarms_with_its_line() {
        let d = testkit::TempDir::new("batcher-cut-land-refused");
        let e = stub_queue(&d, 0, 1, "queue.sh round land: not a fast-forward");
        let r = round_land(&e, &repo_at(&d), "b1").unwrap();
        assert_eq!(r.outcome, LandOutcome::Refused);
        assert_eq!(r.refusal, "queue.sh round land: not a fast-forward");
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "1", "no retry");
    }

    fn argv_stub(d: &Path, out: &str) -> Env {
        let mut e = lifecycle_tests_env(d);
        testkit::write_exe(
            &d.join("queue"),
            &format!("#!/bin/sh\necho \"$@\" >> '{log}'\ncat >> '{log}.in'\nprintf '%s\\n' '{out}'\n", log = d.join("argv").display()),
        );
        e.queue_bin = d.join("queue");
        e
    }

    fn member(id: &str) -> Member {
        Member { id: id.into(), tip: "t".into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: Vec::new() }
    }

    #[test]
    fn an_opened_round_that_names_other_members_is_abandoned() {
        let d = testkit::TempDir::new("batcher-cut-round-open");
        let e = argv_stub(&d, "batch=b1\nmembers=sp-a:t");
        let err = round_open(&e, &repo_at(&d), &d, &[member("sp-a"), member("sp-b")]).unwrap_err();
        assert!(err.contains("abandoned"), "{err}");
        let argv = fs::read_to_string(d.join("argv")).unwrap();
        assert!(argv.lines().any(|l| l.starts_with("round open")), "{argv}");
        assert!(argv.lines().any(|l| l.starts_with("round abandon b1")), "{argv}");
        assert_eq!(round_open(&argv_stub(&d, "batch=b2\nmembers=sp-a:t sp-b:t"), &repo_at(&d), &d, &[member("sp-a"), member("sp-b")]).unwrap(), "b2");
    }

    #[test]
    fn a_collateral_eject_names_no_suites_and_never_rebuilds() {
        let d = testkit::TempDir::new("batcher-cut-round-eject");
        let e = argv_stub(&d, "");
        round_eject(&e, &repo_at(&d), "b1", "sp-a", &["test-a.sh".into()], &[], false).unwrap();
        round_eject(&e, &repo_at(&d), "b1", "sp-b", &["test-a.sh".into()], &[], true).unwrap();
        let argv = fs::read_to_string(d.join("argv")).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        assert!(lines[0].contains("--no-rebuild") && !lines[0].contains("--suites"), "{argv}");
        assert!(lines[1].contains("--no-rebuild") && lines[1].contains("--suites test-a.sh"), "{argv}");
    }

    #[test]
    fn head_on_base_is_ancestry_not_the_exit_code() {
        let d = testkit::TempDir::new("batcher-cut-land-ancestry");
        let g = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&*d).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["commit", "-q", "--allow-empty", "-m", "a"]);
        let a = g(&["rev-parse", "HEAD"]);
        g(&["commit", "-q", "--allow-empty", "-m", "b"]);
        g(&["branch", "local/main", &a]);
        let b = g(&["rev-parse", "HEAD"]);
        let repo = repo_at(&d);
        assert!(head_on_base(&repo, &a));
        assert!(!head_on_base(&repo, &b), "a head past the landing ref has not landed");
    }
}

#[cfg(test)]
mod wait_lock_tests {
    use super::*;

    #[test]
    fn waits_out_a_short_hold_and_gives_up_on_a_long_one() {
        let d = testkit::TempDir::new("batcher-cut-wl");
        let e = super::lifecycle_tests_env(&d);
        let held = try_lock(&e, "r").unwrap().unwrap();
        assert!(wait_lock(&e, "r", 0).unwrap().is_none(), "a held lock with no wait must report busy");
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            drop(held);
        });
        assert!(wait_lock(&e, "r", 10).unwrap().is_some(), "must acquire once the holder releases");
        t.join().unwrap();
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    fn member(id: &str) -> Member {
        Member { id: id.into(), tip: String::new(), title: String::new(), priority: None, express: false, certified_at: 0, stack: BTreeMap::new(), base_fix: false, blocked_by: Vec::new() }
    }

    // Two members that are each fine alone: the merged tree carries a stale matrix and a
    // non-executable new script. One commit repairs both and names both ids.
    #[test]
    fn the_fix_is_one_round_commit_naming_every_member() {
        let dir = testkit::TempDir::new("batcher-integration-fix");
        let wt = dir.join("wt");
        fs::create_dir_all(wt.join("spira")).unwrap();
        fs::create_dir_all(wt.join("docs/test-plan")).unwrap();
        git(&wt, &["init", "-q"]);
        fs::write(wt.join("docs/test-plan/coverage.json"), "old\n").unwrap();
        fs::write(wt.join("spira/plan-matrix.sh"), "#!/usr/bin/env bash\necho new > docs/test-plan/coverage.json\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "base"]);
        let base = git(&wt, &["rev-parse", "HEAD"]).trim().to_string();
        fs::write(wt.join("spira/test-new.sh"), "#!/usr/bin/env bash\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "members"]);

        let env = lifecycle_tests_env(dir.path());
        let fixes = integration_fix(&env, &wt, &base, &[member("sp-a1"), member("sp-b2")]).unwrap();
        assert_eq!(fixes, vec!["chmod +x new scripts", "regenerate the test-plan matrix"]);
        let msg = git(&wt, &["log", "-1", "--format=%B"]);
        assert!(msg.contains("sp-a1") && msg.contains("sp-b2"), "{msg}");
        assert!(git(&wt, &["ls-tree", "HEAD", "spira/test-new.sh"]).starts_with("100755"));
        assert_eq!(fs::read_to_string(wt.join("docs/test-plan/coverage.json")).unwrap(), "new\n");
        assert_eq!(integration_fix(&env, &wt, &base, &[member("sp-a1")]).unwrap(), Vec::<&str>::new(), "a second pass finds nothing and commits nothing");
    }

    #[test]
    fn a_hold_blocks_only_the_same_round_and_only_while_its_bead_is_not_closed() {
        let dir = testkit::TempDir::new("batcher-hold");
        let mut env = lifecycle_tests_env(dir.path());
        let lc = dir.join("spira-lc");
        testkit::write_exe(&lc, "#!/usr/bin/env bash\necho '[{\"id\":\"'$4'\",\"status\":\"'$(cat \"$(dirname \"$0\")/status\")'\"}]'\n");
        env.lc_bin = Some(lc);
        let key = batcher::core::round_key(&[("sp-b".into(), "2".into()), ("sp-a".into(), "1".into())]);
        write_hold(&env, "r", &key, "sp-hold1").unwrap();
        fs::write(dir.join("status"), "open").unwrap();
        assert_eq!(hold_blocking(&env, "r", &key).as_deref(), Some("sp-hold1"));
        let moved = batcher::core::round_key(&[("sp-a".into(), "1".into()), ("sp-b".into(), "3".into())]);
        assert_eq!(hold_blocking(&env, "r", &moved), None, "a moved tip is a different round");
        fs::write(dir.join("status"), "closed").unwrap();
        assert_eq!(hold_blocking(&env, "r", &key), None);
    }
}

#[cfg(test)]
pub(crate) mod base_conflict_tests {
    use super::*;

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// base has f=0; `a` edits only fa, `b` edits only fb... `c` and `d` both rewrite f.
    /// Returns (repo, base, a, b, c, d) with a/b disjoint from base and each other except
    /// that a and b both rewrite `shared` differently (round-only conflict); c rewrites f
    /// against a base that moved f.
    pub fn fixture(d: &Path) -> [String; 5] {
        git(d, &["init", "-q", "-b", "main"]);
        for f in ["f", "shared"] {
            fs::write(d.join(f), "0\n").unwrap();
            git(d, &["add", f]);
        }
        git(d, &["commit", "-q", "-m", "root"]);
        let root = git(d, &["rev-parse", "HEAD"]);
        let branch = |name: &str, file: &str, body: &str| {
            git(d, &["checkout", "-q", "-b", name, &root]);
            fs::write(d.join(file), body).unwrap();
            git(d, &["commit", "-qam", name]);
            git(d, &["rev-parse", "HEAD"])
        };
        let a = branch("a", "shared", "A\n");
        let b = branch("b", "shared", "B\n");
        let c = branch("c", "f", "C\n");
        git(d, &["checkout", "-q", "-b", "base", &root]);
        fs::write(d.join("f"), "base\n").unwrap();
        git(d, &["commit", "-qam", "base moves f"]);
        let base = git(d, &["rev-parse", "HEAD"]);
        [base, a, b, c, root]
    }

    fn repo(d: &Path) -> Repo {
        Repo { name: "spira".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Local }
    }

    #[test]
    fn a_member_clashing_only_with_another_member_is_not_a_base_conflict() {
        let d = testkit::TempDir::new("batcher-cut-bc-round");
        let [base, a, b, _c, _root] = fixture(&d);
        let r = repo(&d);
        // The round merges a, then b clashes with a — yet neither clashes with the base.
        git(&d, &["checkout", "-q", "--detach", &base]);
        git(&d, &["merge", "-q", "--no-edit", &a]);
        assert!(!run_status(Command::new("git").arg("-C").arg(&*d).args(["merge", "--no-edit", &b])), "b must clash with a");
        git(&d, &["merge", "--abort"]);
        assert!(base_conflict(&r, &base, &a).is_none());
        assert!(base_conflict(&r, &base, &b).is_none());
    }

    #[test]
    fn a_member_clashing_with_the_base_is_a_base_conflict() {
        let d = testkit::TempDir::new("batcher-cut-bc-base");
        let [base, _a, _b, c, _root] = fixture(&d);
        assert_eq!(base_conflict(&repo(&d), &base, &c), Some(vec!["f".to_string()]));
    }

    #[test]
    fn an_unresolvable_tip_is_not_a_conflict() {
        let d = testkit::TempDir::new("batcher-cut-bc-bad");
        let [base, ..] = fixture(&d);
        assert!(base_conflict(&repo(&d), &base, "0000000000000000000000000000000000000000").is_none());
    }
}

/// A scratch Env for the unit tests: nothing points anywhere real.
#[cfg(test)]
pub(crate) fn lifecycle_tests_env(dir: &Path) -> Env {
    Env {
        home: dir.to_path_buf(),
        run: dir.to_path_buf(),
        queue_dir: dir.join("queue"),
        db: None,
        bd: "bd".into(),
        forge: PathBuf::from("forge"),
        tsd_bin: None,
        round_vm: dir.join("round-vm"),
        queue_bin: dir.join("queue"),
        rebase_stale_bin: dir.join("rebase-stale"),
        testenv_bin: dir.join("testenv"),
        checks_bin: dir.join("checks"),
        round_slots: None,
        hypervisor: Hypervisor { host_mib: 0, round_vm_mib: 0, runner_mib: 0 },
        poll: std::time::Duration::from_secs(1),
        maxpar: 1,
        wall_secs: 1,
        rust_toolchain: "1.82.0".into(),
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_rows: Default::default(),
        lc_bin: None,
        lc_timeout: 5,
        verdicts: dir.join("verdicts"),
        land_lock_attempts: 3,
        land_lock_wait: Duration::from_millis(1),
        land_unpause: None,
    }
}

#[cfg(test)]
mod hypervisor_tests {
    use super::Hypervisor;

    fn hv(host_mib: u64, round_vm_mib: u64) -> Hypervisor {
        Hypervisor { host_mib, round_vm_mib, runner_mib: 16384 }
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
mod local_tier_tests {
    use super::*;

    fn fixture(tag: &str, script: &str) -> (testkit::TempDir, Env, Repo) {
        let d = testkit::TempDir::new(tag);
        let env = lifecycle_tests_env(d.path());
        testkit::write_exe(&env.testenv_bin, script);
        let repo = Repo { name: "r".into(), path: d.path().to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Forge };
        (d, env, repo)
    }

    #[test]
    fn a_red_tip_is_refused_with_its_locations_and_records_no_pass() {
        let (_d, env, repo) = fixture("lt-red", "#!/bin/sh\necho '   --> src/binder.rs:222:9'\nexit 1\n");
        let e = local_tier(&env, &repo, "abc").unwrap_err();
        assert!(e.contains("src/binder.rs:222:9"), "{e}");
        assert!(!local_pass_path(&env, &repo, "abc").exists());
    }

    #[test]
    fn a_red_tip_names_its_tier_command_and_first_error() {
        let script = "#!/bin/sh\necho '  clippy                           RED     after 3s'\necho '  command (clippy): cargo clippy -- -D warnings'\necho '  first error (clippy): src/binder.rs:222:9: error: unused variable'\nexit 1\n";
        let (_d, env, repo) = fixture("lt-headline", script);
        let e = local_tier(&env, &repo, "abc").unwrap_err();
        let head = e.lines().next().unwrap();
        assert!(head.contains("clippy") && head.contains("cargo clippy -- -D warnings") && head.contains("src/binder.rs:222:9"), "{head}");
    }

    #[test]
    fn a_green_tip_is_recorded_and_not_rerun() {
        let (_d, env, repo) = fixture("lt-green", "#!/bin/sh\necho ran >> \"$0.log\"\nexit 0\n");
        local_tier(&env, &repo, "abc").unwrap();
        assert!(local_pass_path(&env, &repo, "abc").is_file());
        local_tier(&env, &repo, "abc").unwrap();
        let ran = fs::read_to_string(format!("{}.log", env.testenv_bin.display())).unwrap();
        assert_eq!(ran.lines().count(), 1);
    }

    #[test]
    fn a_red_landing_ref_is_a_fault_reported_once_and_a_green_one_is_not() {
        let (_d, env, repo) = fixture("lt-fault", "#!/bin/sh\necho 'tier X red'\nexit 1\n");
        let (ev, first) = landing_ref_fault(&env, &repo, "base1").unwrap();
        assert!(ev.contains("tier X red") && first);
        let (_, again) = landing_ref_fault(&env, &repo, "base1").unwrap();
        assert!(!again);
        let (_d2, env2, repo2) = fixture("lt-nofault", "#!/bin/sh\nexit 0\n");
        assert!(landing_ref_fault(&env2, &repo2, "base1").is_none());
    }

    #[test]
    fn a_missing_testenv_refuses() {
        let (_d, mut env, repo) = fixture("lt-missing", "#!/bin/sh\nexit 0\n");
        env.testenv_bin = env.run.join("no-such-testenv");
        assert!(local_tier(&env, &repo, "abc").is_err());
    }
}

#[cfg(test)]
mod fences_verdict_tests {
    use super::fences_judged_nothing;

    #[test]
    fn an_over_cap_fences_log_judged_nothing_and_a_real_fail_did_not() {
        let capped = "gate: VERDICT=PASS reason=syntax-only branch=b repo=r suite=-\ngate: VERDICT=FAIL reason=over-cap branch=b repo=r suite=- (killed at the 300s hard cap)\n";
        assert!(fences_judged_nothing(capped));
        assert!(fences_judged_nothing("gate: VERDICT=NO_VERDICT reason=died"));
        assert!(!fences_judged_nothing("gate: VERDICT=FAIL reason=fence branch=b repo=r suite=gate-fences"));
    }
}
