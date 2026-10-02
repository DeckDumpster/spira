//! The impure half: git, testenv-batch, the forge and the bead store. Every function here
//! either shells out to a subprocess or touches the filesystem; nothing in `core` does either,
//! so every decision the batcher makes is a fixture `core`'s replay tests can drive without a
//! container, git or the wall clock.
//!
//! Where an operation already has a tested, correct shell implementation (land_mark, a bead
//! reopen, the priority sort), this seam shells out to lib.sh rather than re-deriving the
//! same bd/landstate/event-log side effects a second time in Rust.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub landstate: PathBuf,
    pub db: Option<PathBuf>,
    pub bd: String,
    pub express_label: String,
    pub tsd_bin: Option<PathBuf>,
    pub round_vm: PathBuf,
    /// The `queue` program (by name on the launcher's PATH; a unit test hands in a stub): land-local.
    pub queue_bin: PathBuf,
    /// The `rebase-stale` program (by name on the launcher's PATH).
    pub rebase_stale_bin: PathBuf,
    /// The `landing-pass` program (by name on the launcher's PATH): owns the landstate
    /// ledger's one writer (sp-cnnt6, "wave 4.16") — `land_mark` below shells to its
    /// `mark` subcommand with `$SPIRA_RUN` passed explicitly, never the lib.sh seam.
    pub landing_pass_bin: PathBuf,
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
    /// THE lifecycle switch (DESIGN.md "Lifecycle switch"): off, nothing in this crate runs
    /// spira-lc — not even a probe; `lc_bin` is never consulted to decide it.
    pub lc_enforce: bool,
    /// `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}` — where the round's tree certificate goes
    /// (gate::cert, queue/DESIGN.md §8 D12), resolved exactly as the gate resolves it.
    pub verdicts: PathBuf,
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
// lib.sh dispatch — reuse the tested shell functions for bd/landstate mutations rather than
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

pub fn land_mark(env: &Env, id: &str, state: &str, tip: &str, reason: &str) {
    let mut cmd = Command::new(&env.landing_pass_bin);
    cmd.env("SPIRA_RUN", &env.run).args(["mark", id, state, tip, reason]);
    let _ = run(&mut cmd, "landing-pass mark");
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

pub fn bump_requeue(env: &Env, id: &str, reason: &str) {
    let _ = lib_call(env, "bump_requeue", [id, reason]);
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

// ---------------------------------------------------------------------------------------
// spira-lc: the cutover round's own OPEN-batch lifecycle (sp-o7nbr.4, same contract as
// sp-o7nbr.2's batch.sh _lc_cut_batch/lcq), behind THE switch, `lifecycle_enforce`
// (DESIGN.md "Lifecycle switch"). Off: every function below returns without running
// anything, so the open-batch record carries no batch_id/version — the pre-sp-o7nbr.4
// record. On: `lc_probe` has already refused the cut if the machine is unreachable; after
// that the cut/stack calls stay best-effort and additive (a CAS refusal is reported loudly
// on stderr and leaves batch_id/version unset, never blocking the PR or land_mark).
// ---------------------------------------------------------------------------------------

fn lcq(env: &Env, args: &[&str]) -> Result<String, String> {
    if !env.lc_enforce {
        // Structural, not advisory: off can never reach the binary even if a caller forgets.
        return Err("lifecycle_enforce is off".to_string());
    }
    let bin = env.lc_bin.as_ref().ok_or_else(|| "no spira-lc program".to_string())?;
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).args(args);
    run(&mut cmd, "spira-lc")
}

/// On only: is the machine there to answer? `spira-lc list --state IN_DELIVERY`, parsed —
/// the same probe the queue crate makes. Off: `Ok(())` without running anything.
pub fn lc_probe(env: &Env) -> Result<(), String> {
    if !env.lc_enforce {
        return Ok(());
    }
    let out = lcq(env, &["list", "--state", "IN_DELIVERY"])?;
    serde_json::from_str::<serde_json::Value>(&out).map(|_| ()).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))
}

/// Ensure a bead row exists for every member (never a shortcut to CERTIFIED — that
/// transition is sp-vd9dn's territory) then `spira-lc cut` a brand-new batch. On success
/// the batch's version is exactly the member count (cut's own `MemberAdded` events are
/// the only thing that advances it from 0) — returned so the caller can record it on the
/// open-batch file the same way sp-o7nbr.2's `_lc_cut_batch` does.
pub fn lc_cut_batch(env: &Env, repo: &str, batch_id: &str, head: &str, base: &str, members: &[(String, String)]) -> Option<String> {
    if !env.lc_enforce {
        return None;
    }
    for (id, _) in members {
        let _ = lcq(env, &["create-bead", id]);
    }
    let members_s = members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(",");
    match lcq(env, &["cut", batch_id, "--repo", repo, "--head", head, "--base", base, "--members", &members_s, "--actor", "batcher"]) {
        Ok(_) => Some(members.len().to_string()),
        Err(e) => {
            eprintln!("batcher {repo}: LIFECYCLE: spira-lc cut refused for {batch_id} (lifecycle_enforce is on; the round proceeds without batch_id/version): {e}");
            None
        }
    }
}

/// `id`'s stack (design stacked-dependents-2026-09-28 §1: `{prereq_bead_id: certified_tip}`)
/// off the lifecycle machine's own bead row — `spira-lc show`. Off: unstacked, without
/// running anything (stacking is a lifecycle-machine concept; there is no legacy record of
/// it). On: a row with no `stack` column (a bead the machine has never seen) is unstacked;
/// a failed read or an unparseable reply is also read as unstacked — never a hard error a
/// round must refuse over, since `lc_probe` already proved the machine reachable — but is
/// said loudly on stderr.
fn read_stack(env: &Env, id: &str) -> BTreeMap<String, String> {
    if !env.lc_enforce {
        return BTreeMap::new();
    }
    let out = match lcq(env, &["show", id]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("batcher: LIFECYCLE: spira-lc show {id} failed (lifecycle_enforce is on; read as unstacked): {e}");
            return BTreeMap::new();
        }
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else {
        eprintln!("batcher: LIFECYCLE: spira-lc show {id}: unparsed reply (lifecycle_enforce is on; read as unstacked)");
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
// The certified pool: landstate CERTIFIED records, narrowed to this repo, with title/
// priority/express filled in from the bead store. Same construction as queue-watch's
// snapshot(), which this borrows from directly.
// ---------------------------------------------------------------------------------------

fn read_certified(landstate: &Path) -> Result<Vec<(String, String, u64)>, String> {
    let rd = fs::read_dir(landstate).map_err(|e| format!("{}: {e}", landstate.display()))?;
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if !name.starts_with("sp-") || name.contains(".gate-key") || name.contains(".tmp") {
            continue;
        }
        if let Some((_, ext)) = name.rsplit_once('.') {
            if ext.chars().any(|c| c.is_ascii_alphabetic()) && ext.len() > 2 {
                continue;
            }
        }
        if let Ok(t) = fs::read_to_string(ent.path()) {
            let mut f = t.split_whitespace();
            if f.next() == Some("CERTIFIED") {
                let tip = f.next().unwrap_or("none").to_string();
                let epoch = f.next().and_then(|e| e.parse().ok()).unwrap_or(0);
                out.push((name, tip, epoch));
            }
        }
    }
    out.sort();
    Ok(out)
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

/// Which of `repo`'s own `refs/heads/spira/*` branches exist — the same authority
/// `queue_certified_list` (lib.sh) uses to scope a shared landstate directory to one
/// repository: a branch that exists here is this repo's, regardless of what any bead label
/// says.
fn repo_branch_ids(repo: &Repo) -> Result<std::collections::BTreeSet<String>, String> {
    let out = run(
        Command::new("git").arg("-C").arg(&repo.path).args(["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/*"]),
        "git for-each-ref",
    )?;
    Ok(out.lines().filter_map(|l| l.strip_prefix("spira/")).map(str::to_string).collect())
}

/// A CERTIFIED record is batchable only while its bead is still waiting for a round: OPEN
/// and carrying the submitted label. A stale record of a CLOSED bead (an incident, an ask, a
/// test bead, work already landed) is not a member — the first live cut (2026-09-29) merged
/// nine such branches, one of them `TEST-DRAIN-DEBUG-DELETE-ME` (sp-1346p).
pub fn eligible(status: &str, labels: &[&str], submitted_label: &str) -> bool {
    status == "open" && labels.contains(&submitted_label)
}

/// The certified pool for `repo`: every CERTIFIED landstate record whose branch exists in
/// this repo's own checkout, with title/priority/express filled in from one bulk `bd show`.
pub fn certified_pool(env: &Env, repo: &Repo) -> Result<Vec<Member>, String> {
    let certified = read_certified(&env.landstate)?;
    let branches = repo_branch_ids(repo)?;
    let certified: Vec<_> = certified.into_iter().filter(|(id, _, _)| branches.contains(id)).collect();
    let ids: Vec<String> = certified.iter().map(|(id, _, _)| id.clone()).collect();
    let v = bd_show(env, &ids)?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    let submitted_label = std::env::var("SPIRA_SUBMITTED_LABEL").ok().filter(|l| !l.is_empty()).unwrap_or_else(|| "spira-submitted".into());
    let mut by_id: BTreeMap<String, (Option<u8>, String, bool)> = BTreeMap::new();
    for it in items {
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
        let labels: Vec<&str> = it
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let status = it.get("status").and_then(|x| x.as_str()).unwrap_or("");
        if !eligible(status, &labels, &submitted_label) {
            continue;
        }
        let express = labels.contains(&env.express_label.as_str());
        let priority = it.get("priority").and_then(|p| p.as_u64()).map(|p| p.min(9) as u8);
        let title = it.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
        by_id.insert(id.to_string(), (priority, title, express));
    }
    let mut out = Vec::new();
    for (id, tip, epoch) in certified {
        let Some((priority, title, express)) = by_id.get(&id) else { continue };
        let stack = read_stack(env, &id);
        out.push(Member { id, tip, title: title.clone(), priority: *priority, express: *express, certified_at: epoch, stack });
    }
    Ok(out)
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
    /// when `lifecycle_enforce` is on and `spira-lc cut`/`stack` actually succeeded. queue verdict's lifecycle land walk
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

/// Does `tip` conflict with `base_sha` alone (a real rebase need), or only with the batch
/// this round is accumulating? A throwaway detached worktree, never the batch worktree
/// itself, so a base-only conflict leaves the round's own merge state untouched.
pub fn base_conflict(repo: &Repo, run_dir: &Path, base_sha: &str, tip: &str) -> bool {
    let wt = run_dir.join("worktree").join(format!(".batcher-bc-{}", std::process::id()));
    if !run_status(Command::new("git").arg("-C").arg(&repo.path).args(["worktree", "add", "-q", "--detach"]).arg(&wt).arg(base_sha)) {
        return false;
    }
    let conflicts = !run_status(Command::new("git").arg("-C").arg(&wt).args(["merge", "--no-commit", "--no-ff"]).arg(tip));
    let _ = Command::new("git").arg("-C").arg(&wt).args(["merge", "--abort"]).status();
    let _ = Command::new("git").arg("-C").arg(&repo.path).args(["worktree", "remove", "-f"]).arg(&wt).status();
    conflicts
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

/// Eject one member from the round before it ever reaches CI: reopen its bead (which, being
/// CERTIFIED, withdraws that certification — bead_reopen's own contract) with a note naming
/// every suite it turned red, then record the landstate EJECTED the same way the retired verdict.sh's own
/// CI-side ejection does, so the funnel (cockpit, census) counts a local and a CI ejection the
/// same way. `queue-eject-local` is a distinct reopen cause from the retired verdict.sh's `queue-eject`,
/// so census.sh can tell the two apart.
///
/// The suites also go to bead_reopen's fourth argument, which writes the `<id>.ejected`
/// sidecar, as the retired verdict.sh's own ejection did (sp-p3srm): the gate's re-entry check reads it
/// first, and unlike the EJECTED landstate row it survives the row being overwritten
/// (REBASED, WITHDRAWN) before the bead's next gate.
pub fn eject_member(env: &Env, repo_name: &str, id: &str, tip: &str, suites: &[String]) {
    let suites_csv = suites.join(",");
    let note = format!(
        "Ejected by the merge queue's local attribution (pre-PR): spira/{id} turned red on: {}.",
        suites.join(", ")
    );
    let _ = lib_call(env, "bead_reopen", [id, "queue-eject-local", note.as_str(), suites_csv.as_str()]);
    land_mark(env, id, "EJECTED", tip, &suites_csv);
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

pub fn land_local(env: &Env, repo: &Repo, wt: &Path, head: &str, members: &[(String, String)]) -> Result<LandOutcome, String> {
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
    Ok(classify_land_exit(o.status.code()))
}

// ---------------------------------------------------------------------------------------
// TSD: append this round's record. Best-effort, like land_mark's own TSD write — an
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
mod result_path_tests {
    #[test]
    fn only_open_submitted_beads_are_batch_members() {
        let sub = "spira-submitted";
        assert!(eligible("open", &["repo:spira", sub], sub));
        assert!(!eligible("closed", &["repo:spira", sub], sub), "a closed bead's stale CERTIFIED record is not a member");
        assert!(!eligible("open", &["repo:spira"], sub), "reopened for rework: no longer submitted");
        assert!(!eligible("in_progress", &[sub], sub), "an aeon holds it");
    }

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
    //! The lifecycle switch (DESIGN.md "Lifecycle switch"): off never runs spira-lc; on
    //! runs it, and an unreachable machine fails the probe.
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

    fn env(dir: &Path, lc_bin: Option<PathBuf>, lc_enforce: bool) -> Env {
        Env { lc_bin, lc_enforce, ..super::lifecycle_tests_env(dir) }
    }

    fn members() -> Vec<(String, String)> {
        vec![("sp-a".into(), "aaaa".into()), ("sp-b".into(), "bbbb".into())]
    }

    #[test]
    fn off_never_runs_spira_lc_and_records_no_batch_id() {
        let d = scratch("off");
        // A working, executable spira-lc: presence alone must not turn anything on.
        let e = env(&d, Some(fake_lc(&d, r#"{"bead":{"stack":{"sp-z":"zzzz"}}}"#)), false);
        assert_eq!(lc_probe(&e), Ok(()));
        assert_eq!(lc_cut_batch(&e, "r", "spira/queue/1", "h", "b", &members()), None);
        assert!(read_stack(&e, "sp-a").is_empty());
        assert!(lcq(&e, &["list"]).is_err(), "lcq itself refuses when off");
        assert!(!d.join("lc.log").exists(), "spira-lc must never run with lifecycle_enforce off");

        // The record the cut writes when lc_cut_batch returned None: batch_id/version empty,
        // so neither key is written.
        let ob = OpenBatch { pr: "7".into(), members: members(), owner: "batcher".into(), ..OpenBatch::default() };
        write_open_batch(&e, "r", &ob).unwrap();
        let text = fs::read_to_string(d.join("queue/r/open")).unwrap();
        assert!(!text.contains("batch_id=") && !text.contains("version="), "{text}");
    }

    #[test]
    fn on_runs_spira_lc_and_records_the_batch() {
        let d = scratch("on");
        let e = env(&d, Some(fake_lc(&d, r#"[]"#)), true);
        assert_eq!(lc_probe(&e), Ok(()));
        assert_eq!(lc_cut_batch(&e, "r", "spira/queue/1", "h", "b", &members()), Some("2".into()));
        let log = fs::read_to_string(d.join("lc.log")).unwrap();
        assert!(log.contains("list --state IN_DELIVERY"), "{log}");
        assert!(log.contains("cut spira/queue/1 --repo r --head h --base b --members sp-a:aaaa,sp-b:bbbb"), "{log}");
    }

    #[test]
    fn on_reads_the_stack_off_the_machine() {
        let d = scratch("on-stack");
        let e = env(&d, Some(fake_lc(&d, r#"{"bead":{"stack":{"sp-z":"zzzz"}}}"#)), true);
        assert_eq!(read_stack(&e, "sp-a").get("sp-z").map(String::as_str), Some("zzzz"));
    }

    #[test]
    fn on_unreachable_machine_fails_the_probe() {
        let d = scratch("on-gone");
        let e = env(&d, Some(d.join("nonexistent-spira-lc")), true);
        assert!(lc_probe(&e).is_err());
        let e = env(&d, None, true);
        assert_eq!(lc_probe(&e), Err("no spira-lc program".to_string()));
        // A reachable binary with a non-JSON reply is not an answer either.
        let e = env(&d, Some(fake_lc(&d, "not json")), true);
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
}

#[cfg(test)]
mod eject_tests {
    use super::*;

    #[test]
    fn an_ejected_member_carries_its_suites_to_the_sidecar_and_the_row() {
        let d = testkit::TempDir::new("batcher-cut-eject");
        // A lib.sh that records each call's argv, one call per line, fields tab-separated.
        // land_mark is NOT here (sp-cnnt6, "wave 4.16") — landing-pass owns that write now,
        // a stand-in landing-pass binary below records it instead.
        let log = d.join("calls");
        let rec = |f: &str| format!("{f}() {{ (IFS=$'\\t'; printf '{f}\\t%s\\n' \"$*\") >> '{}'; }}\n", log.display());
        fs::write(d.join("lib.sh"), rec("bead_reopen")).unwrap();
        let mut e = super::lifecycle_tests_env(&d);
        let landing_pass = d.join("landing-pass");
        testkit::write_exe(
            &landing_pass,
            &format!(
                "#!/bin/sh\n{{ printf 'land_mark'; for a in \"$@\"; do printf '\\t%s' \"$a\"; done; printf '\\n'; }} >> '{}'\n",
                log.display()
            ),
        );
        e.landing_pass_bin = landing_pass;
        eject_member(&e, "spira", "sp-m2", "abc", &["test-a.sh".into(), "test-b.sh".into()]);
        let calls = fs::read_to_string(&log).unwrap();
        let lines: Vec<Vec<&str>> = calls.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(lines.len(), 2, "{calls}");
        assert_eq!(lines[0][..3], ["bead_reopen", "sp-m2", "queue-eject-local"]);
        assert_eq!(lines[0][4], "test-a.sh,test-b.sh", "the fourth argument writes <id>.ejected");
        assert_eq!(lines[1], ["land_mark", "mark", "sp-m2", "EJECTED", "abc", "test-a.sh,test-b.sh"]);
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
}

/// A scratch Env for the unit tests: nothing points anywhere real.
#[cfg(test)]
fn lifecycle_tests_env(dir: &Path) -> Env {
    Env {
        home: dir.to_path_buf(),
        run: dir.to_path_buf(),
        queue_dir: dir.join("queue"),
        landstate: dir.join("landstate"),
        db: None,
        bd: "bd".into(),
        express_label: "express".into(),
        tsd_bin: None,
        round_vm: dir.join("round-vm"),
        queue_bin: dir.join("queue"),
        rebase_stale_bin: dir.join("rebase-stale"),
        landing_pass_bin: dir.join("landing-pass"),
        round_slots: None,
        poll_secs: 1,
        maxpar: 1,
        wall_secs: 1,
        rust_toolchain: "1.82.0".into(),
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_bin: None,
        lc_timeout: 5,
        lc_enforce: false,
        verdicts: dir.join("verdicts"),
    }
}
