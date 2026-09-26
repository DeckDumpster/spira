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
    pub git_name: String,
    pub git_email: String,
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

pub fn bead_reopen(env: &Env, id: &str, cause: &str, note: &str) {
    let _ = lib_call(env, "bead_reopen", [id, cause, note]);
}

pub fn bump_requeue(env: &Env, id: &str, reason: &str) {
    let _ = lib_call(env, "bump_requeue", [id, reason]);
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
    }))
}

pub fn write_open_batch(env: &Env, repo: &str, ob: &OpenBatch) -> Result<(), String> {
    let p = open_file(env, repo);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let members = ob.members.iter().map(|(id, tip)| format!("{id}:{tip}")).collect::<Vec<_>>().join(" ");
    let body = format!(
        "pr={}\nhead={}\nbase={}\nmembers={}\nopened={}\nbranch={}\n",
        ob.pr, ob.head, ob.base, members, ob.opened, ob.branch
    );
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
    let ok = run_status(
        Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["-c", &format!("user.name={}", env.git_name), "-c", &format!("user.email={}", env.git_email)])
            .args(["merge", "--no-edit", "--no-ff", "-m", &format!("spira: land {id}")])
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
