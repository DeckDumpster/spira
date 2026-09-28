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
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use batcher::core::{Member, MergeResult, PoolHistory, SuiteOutcome, SuiteRun};

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
    pub forge: PathBuf,
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
    pub testenv_batch: PathBuf,
    pub attribute: PathBuf,
    pub git_name: String,
    pub git_email: String,
    pub lc_bin: Option<PathBuf>,
    pub lc_timeout: u64,
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
    run(&mut cmd, &format!("lib.sh {func}"))
}

pub fn land_mark(env: &Env, id: &str, state: &str, tip: &str, reason: &str) {
    let _ = lib_call(env, "land_mark", [id, state, tip, reason]);
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

// ---------------------------------------------------------------------------------------
// spira-lc: the cutover round's own OPEN-batch lifecycle (sp-o7nbr.4, same contract as
// sp-o7nbr.2's batch.sh _lc_cut_batch/lcq — best-effort and additive, never blocking the
// existing land_mark-based path). No SPIRA_LC_BIN, or a refusal, is logged to stderr and
// returns None; a caller that gets None simply leaves batch_id/version unset on the
// open-batch record, same as a legacy or refused-cut record already does.
// ---------------------------------------------------------------------------------------

fn lcq(env: &Env, args: &[&str]) -> Result<String, String> {
    let bin = env.lc_bin.as_ref().ok_or_else(|| "SPIRA_LC_BIN unset".to_string())?;
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).args(args);
    run(&mut cmd, "spira-lc")
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
            eprintln!("batcher {repo}: spira-lc cut refused for {batch_id}: {e}");
            None
        }
    }
}

/// `spira-lc stack` new members onto an already-OPEN batch — batcher-cut's own
/// pipelining (law-queue-back-pressure-is-an-open-pr). On success the batch's version
/// advances by exactly the new member count, so `prior_version + members.len()` is
/// recorded without a second round trip to read it back.
pub fn lc_stack_batch(env: &Env, repo: &str, batch_id: &str, prior_version: u64, members: &[(String, String)]) -> Option<String> {
    for (id, _) in members {
        let _ = lcq(env, &["create-bead", id]);
    }
    let members_s = members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(",");
    match lcq(env, &["stack", batch_id, "--members", &members_s, "--actor", "batcher"]) {
        Ok(_) => Some((prior_version + members.len() as u64).to_string()),
        Err(e) => {
            eprintln!("batcher {repo}: spira-lc stack refused for {batch_id}: {e}");
            None
        }
    }
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
    let mut by_id: BTreeMap<String, (Option<u8>, String, bool)> = BTreeMap::new();
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
        by_id.insert(id.to_string(), (priority, title, express));
    }
    let mut out = Vec::new();
    for (id, tip, epoch) in certified {
        let Some((priority, title, express)) = by_id.get(&id) else { continue };
        out.push(Member { id, tip, title: title.clone(), priority: *priority, express: *express, certified_at: epoch });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Pool history: this repo's own batch-round TSD rows, so adaptive_n has something to work
// from once at least one round has landed. No prior round: PoolHistory::default(), which
// adaptive_n reads as N=4 — the same bootstrap floor a fresh install starts from anyway.
// ---------------------------------------------------------------------------------------

pub fn pool_history(run_dir: &Path, repo_name: &str, pool_len: usize) -> PoolHistory {
    let path = run_dir.join("tsd").join("batch-round.jsonl");
    let Ok(text) = fs::read_to_string(&path) else { return PoolHistory::default() };
    let mut last_ts: Option<String> = None;
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("repo").and_then(|r| r.as_str()) != Some(repo_name) {
            continue;
        }
        last_ts = v.get("ts").and_then(|t| t.as_str()).map(|s| s.to_string());
        break;
    }
    let Some(ts) = last_ts else { return PoolHistory::default() };
    let Some(then) = parse_iso(&ts) else { return PoolHistory::default() };
    let mins = (now().saturating_sub(then) as f64 / 60.0).max(1.0);
    PoolHistory { certify_rate_per_min: pool_len as f64 / mins, round_duration_mins: mins }
}

fn parse_iso(s: &str) -> Option<u64> {
    // "YYYY-MM-DDTHH:MM:SSZ" -> epoch seconds (Howard Hinnant's civil-to-days, inverse of
    // queue-watch's iso()).
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let hh: i64 = s.get(11..13)?.parse().ok()?;
    let mm: i64 = s.get(14..16)?.parse().ok()?;
    let ss: i64 = s.get(17..19)?.parse().ok()?;
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

// ---------------------------------------------------------------------------------------
// The open-batch record: exactly the key=value shape verdict.sh (and queue-watch) read.
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
    /// separate default to keep in sync). verdict.sh reads it to decide whether a CI red
    /// on this PR is its own attribution's or the summoned batcher persona's (sp-lomk3).
    pub owner: String,
    /// spira-lc's own key for this batch and its current CAS version (sp-o7nbr.4, same
    /// field names as sp-o7nbr.2's `_lc_cut_batch` writes for batch.sh) — present only
    /// when `spira-lc cut`/`stack` actually succeeded. verdict.sh's `_lc_land_batch`/
    /// `_lc_settle_batch` read these generically off this same open-batch file
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

/// Merge one member's tip into the worktree with the queue's own commit message. Aborts and
/// reports Conflict on any failure — a dirty merge state must never be left for the next
/// member's attempt.
pub fn merge_member(env: &Env, wt: &Path, id: &str, tip: &str) -> MergeResult {
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

pub fn push_branch(repo: &Repo, sha: &str, branch: &str) -> Result<(), String> {
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
    run(
        Command::new("git").arg("-C").arg(&repo.path).args(["push", "-q", remote]).arg(format!("{sha}:refs/heads/{branch}")),
        "git push",
    )
    .map(|_| ())
}

pub fn force_push_branch(repo: &Repo, sha: &str, branch: &str) -> Result<(), String> {
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
    run(
        Command::new("git").arg("-C").arg(&repo.path).args(["push", "-q", "-f", remote]).arg(format!("{sha}:refs/heads/{branch}")),
        "git push -f",
    )
    .map(|_| ())
}

pub fn set_branch(repo: &Repo, branch: &str, sha: &str) {
    let _ = Command::new("git").arg("-C").arg(&repo.path).args(["branch", "-f", branch, sha]).status();
}

pub fn head_of(wt: &Path) -> Result<String, String> {
    Ok(run(Command::new("git").arg("-C").arg(wt).args(["rev-parse", "HEAD"]), "git rev-parse HEAD")?.trim().to_string())
}

// ---------------------------------------------------------------------------------------
// testenv-batch: run the full corpus, or a named subset (the re-run of the reds), and
// parse its per-suite result protocol into SuiteRun.
// ---------------------------------------------------------------------------------------

pub fn all_suites(repo: &Repo) -> Vec<String> {
    let dir = repo.path.join("spira");
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with("test-") && name.ends_with(".sh") {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

/// Run `suites` (explicit list — never diff-selected) against `branch` and parse the result
/// protocol testenv-batch.sh's own docs define: `<status> <epoch> <secs> <fp> <mode>
/// <producer> <rc>` per suite, plus its `.out` for the assertion lines classify()'s E-check
/// scans (a line containing "FAIL", the convention every suite here already uses).
pub fn run_suites(env: &Env, repo: &Repo, branch: &str, suites: &[String], results_dir: &Path) -> Result<Vec<SuiteRun>, String> {
    if suites.is_empty() {
        return Ok(vec![]);
    }
    fs::create_dir_all(results_dir).map_err(|e| format!("{}: {e}", results_dir.display()))?;
    let mut cmd = Command::new("bash");
    cmd.env("SPIRA_BATCH_RESULTS", results_dir);
    cmd.arg(&env.testenv_batch).arg("--suites").arg(suites.join(",")).arg(branch).arg(&repo.path);
    let status = cmd.status().map_err(|e| format!("testenv-batch.sh: {e}"))?;
    // Exit 1 means "suites ran, some red" — a real answer, not a fault. Only 2/3 (harness
    // fault: the container never came up, or install failed) refuse to report a verdict.
    match status.code() {
        Some(0) | Some(1) | None => {}
        Some(2) => return Err("testenv-batch.sh: harness fault — container did not come up or died".into()),
        Some(3) => return Err("testenv-batch.sh: harness fault — install failed".into()),
        Some(c) => return Err(format!("testenv-batch.sh: unexpected exit {c}")),
    }
    Ok(suites.iter().map(|s| parse_result(results_dir, s)).collect())
}

fn parse_result(results_dir: &Path, suite: &str) -> SuiteRun {
    let res_path = results_dir.join(format!("{suite}.result"));
    let status = fs::read_to_string(&res_path).ok().and_then(|t| t.split_whitespace().next().map(str::to_string));
    let outcome = match status.as_deref() {
        Some("ok") | Some("skip") | Some("disabled") => SuiteOutcome::Green,
        // Absent — the suite was selected but no result file exists — is unreached, never
        // green (law-absence-needs-a-positive-control): treat as Red so classify() cannot
        // manufacture a pass out of silence.
        _ => SuiteOutcome::Red,
    };
    let mut failing_assertions = Vec::new();
    if outcome == SuiteOutcome::Red {
        if let Ok(out) = fs::read_to_string(results_dir.join(format!("{suite}.out"))) {
            failing_assertions = out.lines().filter(|l| l.contains("FAIL")).map(str::to_string).collect();
        }
    }
    SuiteRun { name: suite.to_string(), outcome, failing_assertions }
}

pub fn red_names(verdicts: &[SuiteRun]) -> Vec<String> {
    verdicts.iter().filter(|s| s.outcome == SuiteOutcome::Red).map(|s| s.name.clone()).collect()
}

// ---------------------------------------------------------------------------------------
// Local attribution (sp-hqoap): attribute.sh (sp-q8xs9) names, per red suite, either the
// member(s) whose tip reproduces it or BASE. Parsed from its stdout protocol rather than
// re-deriving bisection here — that mechanism is attribute.sh's own, already tested.
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attribution {
    /// Suites attribute.sh found red against the base alone — nobody's fault.
    pub base_suites: Vec<String>,
    /// Member id -> the suite(s) attribute.sh attributed to it (attribute.sh's own EJECT
    /// lines); a suite it attributed to more than one member (a bisected interaction) appears
    /// under each.
    pub ejections: BTreeMap<String, Vec<String>>,
}

fn parse_attribution(out: &str) -> Attribution {
    let mut a = Attribution::default();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("ATTR ") {
            let mut it = rest.splitn(2, ' ');
            let suite = it.next().unwrap_or("");
            let remainder = it.next().unwrap_or("");
            let owner_kv = remainder.splitn(4, ' ').next().unwrap_or("");
            if owner_kv.strip_prefix("owner=") == Some("BASE") {
                a.base_suites.push(suite.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("EJECT ") {
            let mut it = rest.splitn(2, ' ');
            let id = it.next().unwrap_or("").to_string();
            let suites: Vec<String> = it.next().unwrap_or("").split(',').map(str::to_string).filter(|s| !s.is_empty()).collect();
            if !id.is_empty() && !suites.is_empty() {
                a.ejections.insert(id, suites);
            }
        }
    }
    a
}

/// Run attribute.sh over the round's own red suites, on this box, and parse who it blames.
/// A non-zero exit (a stubbed-out or broken attribution step) is an error, not an empty
/// attribution — the caller must never read that as "nothing to blame, proceed to a PR"
/// (law-a-control-that-cannot-check-must-refuse).
pub fn attribute(
    env: &Env,
    repo: &Repo,
    round_branch: &str,
    base_sha: &str,
    suites: &[String],
    members: &[String],
) -> Result<Attribution, String> {
    let out = run(
        Command::new("bash")
            .arg(&env.attribute)
            .arg("--round")
            .arg(round_branch)
            .arg("--base")
            .arg(base_sha)
            .arg("--suites")
            .arg(suites.join(","))
            .arg("--members")
            .arg(members.join(","))
            .arg("--repo")
            .arg(&repo.path),
        "attribute.sh",
    )?;
    Ok(parse_attribution(&out))
}

/// Eject one member from the round before it ever reaches CI: reopen its bead (which, being
/// CERTIFIED, withdraws that certification — bead_reopen's own contract) with a note naming
/// every suite it turned red, then record the landstate EJECTED the same way verdict.sh's own
/// CI-side ejection does, so the funnel (cockpit, census) counts a local and a CI ejection the
/// same way. `queue-eject-local` is a distinct reopen cause from verdict.sh's `queue-eject`,
/// so census.sh can tell the two apart.
pub fn eject_member(env: &Env, repo_name: &str, id: &str, tip: &str, suites: &[String]) {
    let suites_csv = suites.join(",");
    let note = format!(
        "Ejected by the merge queue's local attribution (pre-PR): spira/{id} turned red on: {}.",
        suites.join(", ")
    );
    bead_reopen(env, id, "queue-eject-local", &note);
    land_mark(env, id, "EJECTED", tip, &suites_csv);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(env.run.join("landing.log")) {
        use std::io::Write;
        let _ = writeln!(f, "QUEUE LOCAL-EJECT {} repo={repo_name} id={id} suites={suites_csv}", now());
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
        Command::new("bash").arg(env.home.join("incident.sh")).arg("file").arg(&title).arg(&tmp).env("SPIRA_INCIDENT_TYPE", "bug").env(
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

/// The last local corpus verdict for this repo's round — read by queue.sh's own open-batch so
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
    let mut cmd = Command::new("bash");
    cmd.arg(&repo.forge).arg("pr-create").arg(&repo.path).arg(head).arg(base).arg(title);
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
// TSD: append this round's record. Best-effort, like land_mark's own TSD write — an
// unbuilt or missing tsd-write binary means the row stays unwritten, never that the round
// itself fails.
// ---------------------------------------------------------------------------------------

pub fn tsd_append_round(env: &Env, fields: &[(&str, String)]) {
    let Some(bin) = &env.tsd_bin else { return };
    if !bin.is_file() {
        return;
    }
    let mut cmd = Command::new(bin);
    cmd.arg("--family").arg("batch-round").arg("--root").arg(&env.run);
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
        Command::new("bash")
            .arg(env.home.join("bead.sh"))
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
    let start = out.find('{').ok_or_else(|| format!("bead.sh file: no JSON in output: {out}"))?;
    let v: serde_json::Value = serde_json::from_str(&out[start..]).map_err(|e| format!("bead.sh file: unparsed output: {e}"))?;
    let v = match v {
        serde_json::Value::Array(a) => a.into_iter().next().ok_or("bead.sh file: empty JSON array")?,
        o => o,
    };
    v.get("id").and_then(|i| i.as_str()).map(str::to_string).ok_or_else(|| format!("bead.sh file: no id in output: {out}"))
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
