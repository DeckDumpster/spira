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
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use batcher::core::{Member, MergeResult, PoolHistory};

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

#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub run: PathBuf,
    pub queue_dir: PathBuf,
    pub db: Option<PathBuf>,
    pub bd: String,
    pub express_label: String,
    /// `SPIRA_FORGE` — the forge seam `find_repo` hands to `Repo.forge` (bare name on the
    /// launcher's PATH, sp-gypjk; resolved once, here, not re-read per repo).
    pub forge: PathBuf,
    pub tsd_bin: Option<PathBuf>,
    pub round_vm: PathBuf,
    /// The `queue` program (by name on the launcher's PATH; a unit test hands in a stub): land-local.
    pub queue_bin: PathBuf,
    /// The `rebase-stale` program (by name on the launcher's PATH).
    pub rebase_stale_bin: PathBuf,
    /// The round's slot budget for concurrent attribution (DESIGN.md §4.2):
    /// SPIRA_BATCHER_ROUND_SLOTS, else maxpar + 4.
    pub round_slots: Option<u32>,
    /// How often the attribution loop polls the corpus and its reruns (seconds).
    pub poll_secs: u64,
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
    /// How many times `land_local` tries while another queue operation holds the repo's lock.
    pub land_lock_attempts: u32,
    pub land_lock_wait: Duration,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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

fn lcq(env: &Env, args: &[&str]) -> Result<String, String> {
    let bin = env.lc_bin.as_ref().ok_or_else(|| "no spira-lc program".to_string())?;
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).args(args);
    run(&mut cmd, "spira-lc")
}

/// Is the machine there to answer? `spira-lc list --state IN_DELIVERY`, parsed — the same
/// probe the queue crate makes.
pub fn lc_probe(env: &Env) -> Result<(), String> {
    let out = lcq(env, &["list", "--state", "IN_DELIVERY"])?;
    serde_json::from_str::<serde_json::Value>(&out).map(|_| ()).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))
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
        .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// The certified pool: the lifecycle machine's CERTIFIED beads, narrowed to this repo, with
// title/priority/express filled in from the bead store. Same construction as queue-watch's
// snapshot(), which this borrows from directly.
// ---------------------------------------------------------------------------------------

fn read_certified(env: &Env) -> Result<Vec<(String, String, u64)>, String> {
    let out = lcq(env, &["list", "--state", "CERTIFIED"])?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))?;
    let mut certified: Vec<(String, String, u64)> = rows
        .iter()
        .filter_map(|r| {
            let id = r.get("bead_id")?.as_str()?.to_string();
            let tip = r.get("tip").and_then(|t| t.as_str()).unwrap_or("none").to_string();
            let epoch = match r.get("updated_at") {
                Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
                Some(serde_json::Value::String(t)) => t.parse().unwrap_or(0),
                _ => 0,
            };
            Some((id, tip, epoch))
        })
        .collect();
    certified.sort();
    Ok(certified)
}

fn bd_show(env: &Env, ids: &[String]) -> Result<serde_json::Value, String> {
    if ids.is_empty() {
        return Ok(serde_json::Value::Array(vec![]));
    }
    let mut cmd = Command::new(&env.bd);
    if let Some(db) = &env.db {
        cmd.current_dir(db);
    }
    cmd.arg("show").arg("--json").args(ids);
    let text = run(&mut cmd, "bd show")?;
    serde_json::from_str(&text).map_err(|e| format!("bd show: unparsed output: {e}"))
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

/// The certified pool for `repo`: every CERTIFIED bead whose tip is still the
/// live tip of `refs/heads/spira/<id>` in this repo's own checkout, with title/priority/
/// express filled in from one bulk `bd show`.
///
/// Membership is the lifecycle machine's CERTIFIED state alone (sp-mve9i, design §3.4): bd's
/// status and the retired submitted label decide nothing. The sp-1346p shape — a stale
/// record of an incident, an ask, a test bead or work already landed — is a row the machine
/// must not hold CERTIFIED (drop it there), not one this reader second-guesses from bd.
pub fn certified_pool(env: &Env, repo: &Repo) -> Result<Vec<Member>, String> {
    let certified = read_certified(env)?;
    let branches = repo_branch_tips(repo)?;
    let certified: Vec<_> = certified.into_iter().filter(|(id, tip, _)| tip_current(tip, id, &branches)).collect();
    let ids: Vec<String> = certified.iter().map(|(id, _, _)| id.clone()).collect();
    let v = bd_show(env, &ids)?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    let mut by_id: BTreeMap<String, (Option<u8>, String, bool)> = BTreeMap::new();
    let mut texts: BTreeMap<String, (bool, String)> = BTreeMap::new();
    for it in items {
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
        let labels: Vec<&str> = it
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let express = labels.contains(&env.express_label.as_str());
        let priority = it.get("priority").and_then(|p| p.as_u64()).map(|p| p.min(9) as u8);
        let title = it.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let own_ref = it.get("external_ref").and_then(|x| x.as_str()).unwrap_or("");
        let text = format!("{title}\n{}", it.get("description").and_then(|d| d.as_str()).unwrap_or(""));
        texts.insert(id.to_string(), (own_ref.starts_with(&format!("basefail:{}:", repo.name)), text));
        by_id.insert(id.to_string(), (priority, title, express));
    }
    let fixes = base_fix_ids(env, repo, &texts);
    let mut out = Vec::new();
    for (id, tip, epoch) in certified {
        let Some((priority, title, express)) = by_id.get(&id) else { continue };
        let stack = read_stack(env, &id);
        let base_fix = fixes.contains(&id);
        let priority = if base_fix { Some(0) } else { *priority };
        out.push(Member { id, tip, title: title.clone(), priority, express: *express, base_fix, certified_at: epoch, stack });
    }
    Ok(out)
}

/// The certified ids that fix an open base-red bead of `repo`: a bead the basefail bead
/// depends on, one citing it by id, or one carrying the basefail external ref itself. A
/// failed lookup yields none — the lane is an acceleration, never a gate on the round.
fn base_fix_ids(env: &Env, repo: &Repo, texts: &BTreeMap<String, (bool, String)>) -> std::collections::BTreeSet<String> {
    let mut fixes: std::collections::BTreeSet<String> = texts.iter().filter(|(_, (own, _))| *own).map(|(id, _)| id.clone()).collect();
    let mut cmd = Command::new(&env.bd);
    if let Some(db) = &env.db {
        cmd.current_dir(db);
    }
    // bd lists the base-red beads (content); whether one is still open is its lifecycle
    // row's — not terminal (sp-mve9i). A base-red bead with no row, or a machine that cannot
    // answer, accelerates nothing: the lane is never a gate.
    cmd.args(["list", "--json", "--limit", "0", "--all", "--external-contains"]).arg(format!("basefail:{}:", repo.name));
    let Ok(text) = run(&mut cmd, "bd list basefail") else { return fixes };
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str::<serde_json::Value>(&text) else { return fixes };
    let Ok(lc) = lcq(env, &["list"]).and_then(|o| spira_config::lc_state::parse_rows(&o)) else { return fixes };
    let lc = spira_config::lc_state::index(lc);
    let open: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string))
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
                let blocks = d.get("dependency_type").and_then(|t| t.as_str()) != Some("parent-child");
                if let (true, Some(id)) = (blocks, d.get("id").and_then(|i| i.as_str())) {
                    fixes.insert(id.to_string());
                }
            }
        }
    }
    fixes.retain(|id| texts.contains_key(id));
    fixes
}

// ---------------------------------------------------------------------------------------
// Pool history: this repo's own batch-round TSD rows (`tsd_append_round`'s "members"/
// "duration_ms"), never the live pool — that was the tautology, certify_rate and duration
// both derived from the pool being judged. No completed round yet: PoolHistory::default(),
// which adaptive_n reads as N=1 (law-batcher-earns-the-round-by-parity's own floor).
// ---------------------------------------------------------------------------------------

/// `_pool_len` stays in the signature so callers don't have to change; the answer no longer
/// depends on it.
pub fn pool_history(run_dir: &Path, repo_name: &str, _pool_len: usize) -> PoolHistory {
    let path = run_dir.join("tsd").join("batch-round.jsonl");
    let Ok(text) = fs::read_to_string(&path) else { return PoolHistory::default() };
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("repo").and_then(|r| r.as_str()) != Some(repo_name) {
            continue;
        }
        let members = v.get("members").and_then(|m| m.as_str()).and_then(|s| s.parse::<f64>().ok());
        let duration_ms = v.get("duration_ms").and_then(|m| m.as_str()).and_then(|s| s.parse::<f64>().ok());
        let (Some(members), Some(duration_ms)) = (members, duration_ms) else { continue };
        let round_duration_mins = (duration_ms / 1000.0 / 60.0).max(1.0 / 60.0);
        return PoolHistory { certify_rate_per_min: members / round_duration_mins, round_duration_mins };
    }
    PoolHistory::default()
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
    Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
        let o = Command::new("git").arg("-C").arg(&repo.path).args(["diff", "--name-only", &format!("{base_sha}...{tip}")]).output().ok()?;
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
    let o = Command::new("git")
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

/// Takes a member the batcher withdraws out of CERTIFIED on the lifecycle machine (sp-mve9i).
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
    let diff = Command::new("git").arg("-C").arg(&repo.path).args(["diff", &format!("{base_sha}...{tip}")]).output().ok()?;
    if !diff.status.success() {
        return None;
    }
    let mut child = Command::new("git")
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
    let _ = Command::new("git").arg("-C").arg(&repo.path).args(["branch", "-f", branch, sha]).status();
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
        let _ = Command::new("git")
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
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["ls-tree", "-r", "--name-only", branch, "--", "spira/"]),
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
pub fn run_fences(env: &Env, repo: &Repo, branch: &str) -> Result<(), String> {
    let out = Command::new("bash")
        .arg(env.home.join("gate.sh"))
        .arg(branch)
        .arg(&repo.name)
        .env("SPIRA_GATE_SUITES", "off")
        .output()
        .map_err(|e| format!("gate.sh (fences): {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let dir = env.run.join("batch-results");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{}-{}-fences.log", repo.name, now()));
    let _ = fs::write(&path, &text);
    Err(path.display().to_string())
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

/// Eject one member from the round before it ever reaches CI: reopen its bead and withdraw its
/// certification on the lifecycle machine ([`lc_withdraw`]) with a note naming
/// every suite it turned red, with a distinct cause so census can tell a local ejection from a CI one. `queue-eject-local` is a distinct reopen cause from the retired verdict.sh's `queue-eject`,
/// so census.sh can tell the two apart.
///
/// The suites also go to bead_reopen's fourth argument, which writes the `<id>.ejected`
/// sidecar, as the retired verdict.sh's own ejection did (sp-p3srm): the gate's re-entry check reads it
/// first.
pub fn eject_member(env: &Env, repo_name: &str, id: &str, suites: &[String], first_fails: &[(String, String)]) {
    let suites_csv = suites.join(",");
    let note = format!(
        "Ejected by the merge queue's local attribution (pre-PR): spira/{id} turned red on: {}.{}",
        suites.join(", "),
        first_fails.iter().map(|(s, l)| format!(" First FAIL, {s}: {l}")).collect::<String>()
    );
    let _ = lib_call(env, "bead_reopen", [id, "queue-eject-local", note.as_str(), suites_csv.as_str()]);
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

/// The first line of a suite's output that reports a failure, trimmed and bounded.
pub fn first_fail_line(out: &str) -> Option<String> {
    let l = out.lines().map(str::trim).find(|l| l.starts_with("FAIL") || l.contains("FAIL:") || l.starts_with("not ok"))?;
    Some(l.chars().take(300).collect())
}

/// The first FAIL line of `suite`'s output in the round's results, for an ejection's reason.
pub fn suite_first_fail(results_dir: &Path, suite: &str) -> Option<String> {
    first_fail_line(&fs::read_to_string(locate(results_dir, suite, "out")?).ok()?)
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
            format!("basefail-local:{}:{}:{reason}", repo.name, suites.join(",")),
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
    let git = |args: &[&str]| run(Command::new("git").arg("-C").arg(wt).args(args), "git");
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
    let mut c = Command::new("git");
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

/// `path`, when given, is the child's PATH — the seam tests use instead of mutating the process PATH.
fn forge_pr_create_on(repo: &Repo, head: &str, base: &str, title: &str, body: &str, path: Option<&std::ffi::OsStr>) -> Result<String, String> {
    // A bare name (the default, `forge`, since sp-yv4b3) is the launcher-PATH program, run
    // directly; a configured SPIRA_FORGE path is run with bash, as batch.sh did.
    let mut cmd = if repo.forge.components().count() == 1 {
        Command::new(&repo.forge)
    } else {
        let mut c = Command::new("bash");
        c.arg(&repo.forge);
        c
    };
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
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
// all of it queue land-local's own contract, never re-derived here.
// ---------------------------------------------------------------------------------------

/// Certify the round: the full corpus just ran green on `head`, so record `verdict=GREEN
/// source=round` for `head`'s tree (gate::cert; queue/DESIGN.md §8 D12). `queue land-local`
/// lands only a tree a gate PASS or a round GREEN certified, and a round head is a merge of
/// many members that no per-branch gate ever judged. Err when the tree cannot be resolved or
/// the certificate cannot be written; the caller refuses the round rather than land on a
/// certificate that is not there.
pub fn certify_round(env: &Env, repo: &Repo, head: &str, round_branch: &str) -> Result<PathBuf, String> {
    let tree = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["rev-parse", "--verify", "-q"]).arg(format!("{head}^{{tree}}")),
        "git rev-parse <head>^{tree}",
    )?
    .trim()
    .to_string();
    let when = run(Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]), "date").map(|s| s.trim().to_string()).unwrap_or_else(|_| "-".into());
    let c = gate::cert::Cert {
        source: gate::cert::Source::Round,
        tree,
        repo: repo.name.clone(),
        rev: head.to_string(),
        branch: round_branch.to_string(),
        by: "batcher".into(),
        when,
        at: now(),
        harness: "-".into(),
        suites: "full-corpus".into(),
    };
    gate::cert::write(&env.verdicts, &c).map_err(|e| format!("round certificate for {head}: {e}"))
}

/// Land `head` locally via `queue land-local`, under the round lock this crate's own
/// `try_lock` already holds — SPIRA_QUEUE_LOCK_HELD=1 tells land-local to skip its own flock
/// rather than block forever on a lock this same process already owns.
/// Exit 1 is land-local's own refusal (non-fast-forward, no --with-bins corpus, a concurrent
/// mover) — reported, not an error, since "refused, nothing changed" is exactly the same benign
/// outcome `try_lock`'s own None already models for this crate's other refusals. Exit 3 is a
/// deploy fault: the members landed, only the release was not activated.
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

/// What one `land_local` call came to: the outcome, and the line that explains a refusal.
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
pub fn land_local(env: &Env, repo: &Repo, wt: &Path, head: &str, members: &[(String, String)]) -> Result<LandRun, String> {
    let attempts = env.land_lock_attempts.max(1);
    let mut last = LandRun { outcome: LandOutcome::Refused, refusal: String::new() };
    for n in 1..=attempts {
        last = land_local_once(env, repo, wt, head, members)?;
        let busy = last.outcome == LandOutcome::Refused && last.refusal.contains(LOCK_BUSY);
        if !busy || n == attempts {
            break;
        }
        println!("batcher {}: queue land-local refused ({}) — retry {n}/{attempts}", repo.name, last.refusal);
        std::thread::sleep(env.land_lock_wait);
    }
    Ok(last)
}

fn land_local_once(env: &Env, repo: &Repo, wt: &Path, head: &str, members: &[(String, String)]) -> Result<LandRun, String> {
    use std::io::Write;
    let members_text = members.iter().fold(String::new(), |mut acc, (id, tip)| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "{id}:{tip}");
        acc
    });
    let mut cmd = Command::new(&env.queue_bin);
    cmd.arg("land-local").arg(&repo.name);
    cmd.arg("--head").arg(head);
    cmd.arg("--members-file").arg("-");
    cmd.arg("--worktree").arg(wt);
    cmd.env("SPIRA_QUEUE_LOCK_HELD", "1");
    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("queue land-local: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(members_text.as_bytes()).map_err(|e| format!("queue land-local: members on stdin: {e}"))?;
    }
    let o = child.wait_with_output().map_err(|e| format!("queue land-local: {e}"))?;
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    if !out.is_empty() {
        print!("{out}");
    }
    if !err.is_empty() {
        eprint!("{err}");
    }
    Ok(LandRun { outcome: classify_land_exit(o.status.code()), refusal: refusal_line(&err) })
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
    let mut cmd = Command::new(bin);
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
        Command::new("bead.sh").args(["file", &title, "--for", "batcher", "--repo", &repo.name, "--body-file"]).arg(&tmp).arg("--json"),
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
    _file: fs::File,
}

/// None means another operation already holds the lock — not an error, the caller should
/// simply skip this pass, same as batch.sh's own `flock -n` behaviour.
pub fn try_lock(env: &Env, repo: &str) -> Result<Option<Lock>, String> {
    let dir = env.queue_dir.join(repo);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("lock");
    let f = fs::OpenOptions::new().create(true).write(true).truncate(false).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        Ok(Some(Lock { _file: f }))
    } else {
        Ok(None)
    }
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
case \"$1\" in pr-create) cat >/dev/null; echo 42 ;; *) exit 1 ;; esac
",
        );
        let real_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path = dir.path().as_os_str().to_os_string();
        path.push(":");
        path.push(&real_path);

        let repo = Repo { name: "r".into(), path: PathBuf::from("/tmp/r"), base: "local/main".into(), forge: PathBuf::from("forge"), land: Land::Forge };
        let pr = forge_pr_create_on(&repo, "head", "base", "title", "body", Some(&path)).expect("forge_pr_create should reach the stub");
        assert_eq!(pr, "42");
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
        let m = Member { id: "m1".into(), tip, title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default() };
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
mod pool_history_tests {
    use super::*;
    use batcher::core::adaptive_n;

    fn tmpdir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("batcher-cut-poolhist-{tag}"))
    }

    fn write_round(dir: &Path, repo: &str, members: &str, duration_ms: &str) {
        let tsd = dir.join("tsd");
        fs::create_dir_all(&tsd).unwrap();
        let line = format!(
            r#"{{"ts":"2026-09-27T00:00:00Z","host":"h","family":"batch-round","repo":"{repo}","verdict":"green","members":"{members}","duration_ms":"{duration_ms}","base":"deadbeef"}}"#
        );
        fs::write(tsd.join("batch-round.jsonl"), format!("{line}\n")).unwrap();
    }

    #[test]
    fn no_history_defaults_and_adaptive_n_is_one_not_four() {
        let d = tmpdir("none");
        let hist = pool_history(&d, "spira", 30);
        assert_eq!(hist, PoolHistory::default());
        assert_eq!(adaptive_n(hist), 1);
    }

    // SEEN RED on today's code: adaptive_n(pool_history(..., n)) always came back as n
    // itself, clamped — this asserted 7 with pool_len=30 and got 30.
    #[test]
    fn rate_comes_from_the_last_rounds_own_record_not_the_live_pool() {
        let d = tmpdir("real");
        write_round(&d, "spira", "7", "600000"); // 7 members landed over 10 minutes
        let hist = pool_history(&d, "spira", 30);
        assert_eq!(adaptive_n(hist), 7);
        assert_ne!(adaptive_n(hist), 30_u32.clamp(4, 30), "must not equal clamp(pool_len, 4, 30)");

        // Same history, a different live pool passed in: the answer does not move.
        let hist_other = pool_history(&d, "spira", 2);
        assert_eq!(adaptive_n(hist_other), adaptive_n(hist));
    }

    #[test]
    fn only_this_repos_own_rows_count() {
        let d = tmpdir("otherrepo");
        write_round(&d, "other", "20", "60000");
        assert_eq!(pool_history(&d, "spira", 5), PoolHistory::default());
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
    /// `list --state CERTIFIED` (the given certified ids at their live tips), `list` (every
    /// row in `lc_rows`) and `show`, and a bd stand-in whose `list` honours `--status` the way
    /// bd does — so a reader that still filtered on bd status would see bd's answer.
    fn pool_fixture(tag: &str, certified: &[&str], lc_rows: &[(&str, &str)], bd_rows: &[(&str, &str, &str, &str)]) -> (testkit::TempDir, testkit::TempDir, Env, Repo) {
        let d = tmp_repo(tag);
        let stubs = testkit::TempDir::new(&format!("batcher-cut-pool-stubs-{tag}"));
        let mut cert = Vec::new();
        for id in certified {
            let tip = branch(&d, id, id);
            cert.push(format!(r#"{{"bead_id":"{id}","state":"CERTIFIED","tip":"{tip}","updated_at":5}}"#));
        }
        let all: Vec<String> = lc_rows.iter().map(|(id, st)| format!(r#"{{"bead_id":"{id}","state":"{st}","holds":[]}}"#)).collect();
        fs::write(stubs.join("lc-certified"), format!("[{}]", cert.join(","))).unwrap();
        fs::write(stubs.join("lc-all"), format!("[{}]", all.join(","))).unwrap();
        let row = |(id, status, ext, body): &(&str, &str, &str, &str)| {
            format!(r#"{{"id":"{id}","status":"{status}","title":"t {id}","priority":2,"labels":["repo:r"],"external_ref":"{ext}","description":"{body}"}}"#)
        };
        for r in bd_rows {
            fs::write(stubs.join(format!("show-{}", r.0)), row(r)).unwrap();
        }
        // `list` is only ever `--external-contains basefail:…`: the base-red rows.
        let live: Vec<String> = bd_rows.iter().filter(|r| !r.2.is_empty() && r.1 != "closed").map(row).collect();
        let every: Vec<String> = bd_rows.iter().filter(|r| !r.2.is_empty()).map(row).collect();
        fs::write(stubs.join("list-live"), format!("[{}]", live.join(","))).unwrap();
        fs::write(stubs.join("list-all"), format!("[{}]", every.join(","))).unwrap();
        let sd = stubs.path().display().to_string();
        testkit::write_exe(
            stubs.join("spira-lc"),
            &format!("#!/bin/bash\ncase \"$1 $2\" in \"list --state\") cat {sd}/lc-certified ;; \"list \") cat {sd}/lc-all ;; show*) echo '{{\"bead\":{{\"bead_id\":\"'$2'\"}}}}' ;; esac\n"),
        );
        testkit::write_exe(
            stubs.join("bd"),
            &format!(
                "#!/bin/bash\nif [ \"$1\" = show ]; then shift 2; o=; for i in \"$@\"; do [ -f {sd}/show-$i ] && o=\"$o${{o:+,}}$(cat {sd}/show-$i)\"; done; echo \"[$o]\"; exit 0; fi\nfor a in \"$@\"; do [ \"$a\" = --status ] && {{ cat {sd}/list-live; exit 0; }}; done\ncat {sd}/list-all\n"
            ),
        );
        let mut env = super::lifecycle_tests_env(stubs.path());
        env.lc_bin = Some(stubs.join("spira-lc"));
        env.bd = stubs.join("bd").display().to_string();
        let repo = Repo { name: "r".into(), path: d.path().to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: Land::Forge };
        (d, stubs, env, repo)
    }

    /// sp-mve9i: the pool is the lifecycle machine's CERTIFIED rows. bd's status — here a
    /// bd row someone closed by hand, carrying no submitted label — decides nothing.
    #[test]
    fn a_certified_bead_is_a_member_whatever_bd_says_its_status_is() {
        let (_d, _s, env, repo) = pool_fixture("lc-member", &["sp-c1"], &[("sp-c1", "CERTIFIED")], &[("sp-c1", "closed", "", "")]);
        let ids: Vec<String> = certified_pool(&env, &repo).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["sp-c1"]);
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
    fn the_certified_pool_is_the_machines_certified_beads() {
        let d = scratch("certified");
        let reply = r#"[{"bead_id":"sp-b","tip":"bbbb","updated_at":1790000002},{"bead_id":"sp-a","tip":"aaaa","updated_at":"1790000001"},{"bead_id":"sp-n","tip":null,"updated_at":null}]"#;
        let e = env(&d, Some(fake_lc(&d, reply)));
        assert_eq!(
            read_certified(&e).unwrap(),
            vec![("sp-a".to_string(), "aaaa".to_string(), 1790000001), ("sp-b".into(), "bbbb".into(), 1790000002), ("sp-n".into(), "none".into(), 0)]
        );
        let log = fs::read_to_string(d.join("lc.log")).unwrap();
        assert!(log.contains("list --state CERTIFIED"), "{log}");
        assert!(read_certified(&env(&d, Some(fake_lc(&d, "not json")))).unwrap_err().contains("unparsed"));
        assert!(read_certified(&env(&d, None)).is_err());
    }

    #[test]
    fn runs_spira_lc_and_records_the_batch() {
        let d = scratch("cut");
        let e = env(&d, Some(fake_lc(&d, r#"[]"#)));
        assert_eq!(lc_probe(&e), Ok(()));
        assert_eq!(lc_cut_batch(&e, "r", "spira/queue/1", "h", "b", &members()), Some("2".into()));
        let log = fs::read_to_string(d.join("lc.log")).unwrap();
        assert!(log.contains("list --state IN_DELIVERY"), "{log}");
        assert!(log.contains("cut spira/queue/1 --repo r --head h --base b --members sp-a:aaaa,sp-b:bbbb"), "{log}");
    }

    #[test]
    fn reads_the_stack_off_the_machine() {
        let d = scratch("stack");
        let e = env(&d, Some(fake_lc(&d, r#"{"bead":{"stack":{"sp-z":"zzzz"}}}"#)));
        assert_eq!(read_stack(&e, "sp-a").get("sp-z").map(String::as_str), Some("zzzz"));
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
    fn a_green_round_certifies_exactly_its_heads_tree() {
        let d = testkit::TempDir::new("batcher-cut-cert");
        git(&d, &["init", "-q"]);
        for (f, body) in [("a", "1"), ("b", "2")] {
            fs::write(d.join(f), body).unwrap();
            git(&d, &["add", f]);
            git(&d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", f]);
        }
        let head = git(&d, &["rev-parse", "HEAD"]);
        let tree = git(&d, &["rev-parse", "HEAD^{tree}"]);
        let older = git(&d, &["rev-parse", "HEAD~1^{tree}"]);
        let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "local/main".into(), forge: PathBuf::new(), land: Land::Local };
        let mut e = super::lifecycle_tests_env(&d);
        e.verdicts = d.join("verdicts");
        let p = certify_round(&e, &repo, &head, "spira/batcher-attr/x").unwrap();
        let text = fs::read_to_string(&p).unwrap();
        let c = gate::cert::certifies(&text, "spira", &tree).expect("certifies the head's tree");
        assert_eq!(c.source, gate::cert::Source::Round);
        assert_eq!(c.rev, head);
        assert!(gate::cert::certifies(&text, "spira", &older).is_none());
        assert!(gate::cert::path(&e.verdicts, "spira", &older).map(|p| !p.exists()).unwrap_or(true));
        assert!(certify_round(&e, &repo, "no-such-rev", "x").is_err(), "an unresolvable head certifies nothing");
        let _ = fs::remove_dir_all(&d);
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
        eject_member(&e, "spira", "sp-m2", &["test-a.sh".into(), "test-b.sh".into()], &[("test-a.sh".into(), "FAIL widget".into())]);
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
                "#!/bin/bash\necho \"$*\" >> '{}'\ncase \"$1\" in show) echo '{{\"bead\":{{\"bead_id\":\"'$2'\",\"state\":\"{state}\",\"version\":4}}}}' ;; esac\nexit 0\n",
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
                eject_member(&e, "spira", "sp-a", &["test-a.sh".into()], &[]);
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

    /// A member the machine no longer holds CERTIFIED (resubmitted since, or already
    /// returned) is left alone: nothing to withdraw.
    #[test]
    fn a_member_not_certified_on_the_machine_is_not_touched() {
        let d = testkit::TempDir::new("batcher-cut-lcw-rework");
        fs::write(d.join("lib.sh"), "bead_reopen() { :; }\n").unwrap();
        let mut e = super::lifecycle_tests_env(&d);
        e.lc_bin = Some(lc_stub(&d, "REWORK"));
        eject_member(&e, "spira", "sp-a", &[], &[]);
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
                "#!/bin/sh\ncat >/dev/null\nc=$(cat '{n}' 2>/dev/null || echo 0)\nc=$((c+1)); echo $c > '{n}'\nif [ $c -le {busy_first} ]; then echo 'queue.sh land-local: another queue operation holds the lock for spira' >&2; exit 1; fi\n[ -n '{then_msg}' ] && echo '{then_msg}' >&2\nexit {then_exit}\n",
                n = n.display()
            ),
        );
        e.queue_bin = d.join("queue");
        e
    }

    fn repo_at(d: &Path) -> Repo {
        Repo { name: "spira".into(), path: d.to_path_buf(), base: "local/main".into(), forge: d.to_path_buf(), land: Land::Local }
    }

    #[test]
    fn a_held_lock_retries_then_lands() {
        let d = testkit::TempDir::new("batcher-cut-land-retry");
        let e = stub_queue(&d, 2, 0, "");
        let r = land_local(&e, &repo_at(&d), &d, "abc", &[]).unwrap();
        assert_eq!(r.outcome, LandOutcome::Landed);
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "3");
    }

    #[test]
    fn a_lock_held_past_the_bound_is_a_refusal_naming_the_line() {
        let d = testkit::TempDir::new("batcher-cut-land-held");
        let e = stub_queue(&d, 99, 0, "");
        let r = land_local(&e, &repo_at(&d), &d, "abc", &[]).unwrap();
        assert_eq!(r.outcome, LandOutcome::Refused);
        assert!(r.refusal.contains("holds the lock"), "{}", r.refusal);
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "3", "bounded by land_lock_attempts");
    }

    #[test]
    fn a_refusal_without_the_lock_is_final_and_alarms_with_its_line() {
        let d = testkit::TempDir::new("batcher-cut-land-refused");
        let e = stub_queue(&d, 0, 1, "queue.sh land-local: not a fast-forward");
        let r = land_local(&e, &repo_at(&d), &d, "abc", &[]).unwrap();
        assert_eq!(r.outcome, LandOutcome::Refused);
        assert_eq!(r.refusal, "queue.sh land-local: not a fast-forward");
        assert_eq!(fs::read_to_string(d.join("n")).unwrap().trim(), "1", "no retry");
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
        Member { id: id.into(), tip: String::new(), title: String::new(), priority: None, express: false, certified_at: 0, stack: BTreeMap::new(), base_fix: false }
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
        let bd = dir.join("bd");
        testkit::write_exe(&bd, "#!/usr/bin/env bash\necho '[{\"id\":\"'$3'\",\"status\":\"'$(cat \"$(dirname \"$0\")/status\")'\"}]'\n");
        env.bd = bd.display().to_string();
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
        express_label: "express".into(),
        forge: PathBuf::from("forge"),
        tsd_bin: None,
        round_vm: dir.join("round-vm"),
        queue_bin: dir.join("queue"),
        rebase_stale_bin: dir.join("rebase-stale"),
        round_slots: None,
        poll_secs: 1,
        maxpar: 1,
        wall_secs: 1,
        rust_toolchain: "1.82.0".into(),
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_bin: None,
        lc_timeout: 5,
        verdicts: dir.join("verdicts"),
        land_lock_attempts: 3,
        land_lock_wait: Duration::from_millis(1),
    }
}
