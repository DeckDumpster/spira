//! The impure half: read one repo's queue state, the bead store and the forge into a
//! `Snapshot`. Every read that fails becomes an entry in `Snapshot::errors` — never an empty
//! value — so the core can say "blind" rather than "quiet".

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::{Batch, Bead, Ci, Member, Outcome, PrState, Publish, PublishOutcome, RepoState, Snapshot};

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
    pub forge: PathBuf,
    /// `true` for a `queue.local` repo: `base` is a LOCAL branch (`local/main`) already in
    /// this checkout, never a remote to fetch (see `read_on_base`).
    pub local: bool,
}

#[derive(Clone, Debug)]
pub struct Env {
    pub queue_dir: PathBuf,
    pub db: Option<PathBuf>,
    /// `spira-lc` — the one binary allowed to touch `spira_lifecycle` (see `read_certified`).
    pub lc_bin: Option<PathBuf>,
    pub lc_timeout: u64,
}

/// `key=value` lines, as the queue writes its open-batch file. Ok(None) when absent.
fn read_kv(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(
            t.lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect(),
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn members(s: &str) -> Vec<Member> {
    s.split_whitespace()
        .map(|t| match t.split_once(':') {
            Some((id, tip)) => Member { id: id.into(), tip: tip.into() },
            None => Member { id: t.into(), tip: String::new() },
        })
        .collect()
}

pub fn read_batch(dir: &Path) -> Result<Option<Batch>, String> {
    let Some(kv) = read_kv(&dir.join("open"))? else { return Ok(None) };
    let pr = kv.get("pr").cloned().unwrap_or_default();
    if pr.is_empty() {
        // The queue writes the file before it knows the PR number; an open file with no PR
        // is a cut in progress, not a batch anyone can watch yet.
        return Ok(None);
    }
    Ok(Some(Batch {
        pr,
        head: kv.get("head").cloned().unwrap_or_default(),
        branch: kv.get("branch").cloned().unwrap_or_default(),
        members: members(kv.get("members").map(String::as_str).unwrap_or("")),
    }))
}

/// The `publish` record queue.sh's `_lc_publish_step` writes for a queue.local repo, once its
/// PR number is known — written atomically (a temp file renamed into place), unlike `open`,
/// so there is no "cut in progress" partial state to filter out here.
pub fn read_publish(dir: &Path) -> Result<Option<Publish>, String> {
    let Some(kv) = read_kv(&dir.join("publish"))? else { return Ok(None) };
    let pr = kv.get("pr").cloned().unwrap_or_default();
    if pr.is_empty() {
        return Ok(None);
    }
    Ok(Some(Publish {
        pr,
        head: kv.get("head").cloned().unwrap_or_default(),
        branch: kv.get("branch").cloned().unwrap_or_default(),
    }))
}

pub fn read_bisect(dir: &Path) -> Result<Option<(Vec<String>, u64)>, String> {
    let p = dir.join("bisect");
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let Some(first) = text.lines().find(|l| !l.trim().is_empty()) else { return Ok(None) };
    // Line 1 is "<base-sha> id:tip id:tip ..." (sp-55j4m): the base commit the split was
    // made against, recorded so batch.sh can tell whether the group is still valid. Watching
    // only reports membership, so skip that leading field.
    let members_field = first.split_once(' ').map_or("", |(_, rest)| rest);
    let mtime = fs::metadata(&p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(Some((members(members_field).into_iter().map(|m| m.id).collect(), mtime)))
}

/// Every bead `spira-lc` reports CERTIFIED — `spira_lifecycle` is the one place this state
/// lives now, and `spira-lc` is the one binary allowed to query it (design intent #6), so
/// this shells out rather than reading a directory or re-deriving the query in SQL here.
/// Records are shared across repos; the caller narrows by the bead's `repo:` label.
pub fn read_certified(env: &Env) -> Result<Vec<String>, String> {
    lc_list(env, &["--state", "CERTIFIED", "--snapshot"])
}

/// Every bead whose lifecycle row carries Express.
pub fn read_express(env: &Env) -> Result<BTreeSet<String>, String> {
    Ok(lc_list(env, &["--express", "--live", "--snapshot"])?.into_iter().collect())
}

fn lc_list(env: &Env, filter: &[&str]) -> Result<Vec<String>, String> {
    let bin = env.lc_bin.as_ref().ok_or_else(|| "SPIRA_LC_BIN unset".to_string())?;
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).arg("list").args(filter);
    let what = format!("spira-lc list {}", filter.join(" "));
    let text = run(&mut cmd, &what)?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("spira-lc list: unparsed output: {e}"))?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    let mut out: Vec<String> = items.into_iter().filter_map(|it| it.get("bead_id").and_then(|x| x.as_str()).map(str::to_string)).collect();
    out.sort();
    Ok(out)
}

fn run(cmd: &mut Command, what: &str) -> Result<String, String> {
    let o = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let err = err.lines().last().unwrap_or("").trim();
        return Err(format!("{what} exited {}{}", o.status.code().unwrap_or(-1), if err.is_empty() { String::new() } else { format!(": {err}") }));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

/// Like `run`, but a dropped pooled Dolt connection ("invalid connection" — the Go driver
/// detected it dead before sending the query, so it never ran) is retried once rather than
/// surfaced (sp-ydog2).
fn run_bd(cmd: &mut Command, what: &str) -> Result<String, String> {
    match run(cmd, what) {
        Err(e) if e.contains("invalid connection") => run(cmd, what),
        r => r,
    }
}

#[allow(clippy::type_complexity)]
pub fn read_beads(env: &Env, ids: &BTreeSet<String>, express: &BTreeSet<String>) -> Result<(BTreeMap<String, Bead>, BTreeMap<String, String>), String> {
    let mut beads = BTreeMap::new();
    let mut repos = BTreeMap::new();
    if ids.is_empty() {
        return Ok((beads, repos));
    }
    let bin = env.lc_bin.as_ref().ok_or_else(|| "SPIRA_LC_BIN unset".to_string())?;
    let csv = ids.iter().map(String::as_str).collect::<Vec<_>>().join(",");
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).args(["plan", "items", "--ids", &csv]);
    let text = run_bd(&mut cmd, "spira-lc plan items")?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("spira-lc plan items: unparsed output: {e}"))?;
    let Some(serde_json::Value::Array(items)) = v.get("items").cloned() else {
        return Err("spira-lc plan items: no items array".into());
    };
    for it in items {
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
        let labels: Vec<&str> = it
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        if let Some(r) = labels.iter().find_map(|l| l.strip_prefix("repo:")) {
            repos.insert(id.to_string(), r.to_string());
        }
        beads.insert(
            id.to_string(),
            Bead {
                priority: it.get("priority").and_then(|p| p.as_u64()).map(|p| p.min(9) as u8),
                title: it.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                express: express.contains(id),
            },
        );
    }
    Ok((beads, repos))
}

fn forge(
    repo: &Repo,
    sub: &str,
    pr: &str,
    branch: Option<&str>,
    path: Option<&std::ffi::OsStr>,
) -> Result<String, String> {
    // A bare name (the release's `forge` binary, default since sp-yv4b3) is exec'd by name
    // on PATH; a configured path via bash.
    let mut c = if repo.forge.components().count() == 1 {
        Command::new(&repo.forge)
    } else {
        let mut c = Command::new("bash");
c.envs(spira_config::release_env::child_path_env_for_process());
        c.arg(&repo.forge);
        c
    };
    if let Some(p) = path {
        c.env("PATH", p);
    }
    c.arg(sub).arg(&repo.path).arg(pr);
    if let Some(b) = branch {
        c.arg(b);
    }
    let out = run(
        &mut c,
        &format!("forge {sub} {pr}"),
    )?;
    Ok(out.lines().next().unwrap_or("").trim().to_string())
}

pub fn read_ci(repo: &Repo, pr: &str, branch: &str) -> Result<Ci, String> {
    read_ci_on(repo, pr, branch, None)
}

/// `path`, when given, is the child's PATH — the seam tests use instead of mutating the process PATH.
pub fn read_ci_on(repo: &Repo, pr: &str, branch: &str, path: Option<&std::ffi::OsStr>) -> Result<Ci, String> {
    match forge(repo, "check-status", pr, Some(branch), path)?.as_str() {
        "pending" => Ok(Ci::Pending),
        "green" => Ok(Ci::Green),
        "red" => Ok(Ci::Red),
        k @ ("harness_fault" | "provision_fault") => Ok(Ci::Fault(k.to_string())),
        other => Err(format!("forge check-status {pr}: unrecognised answer {other:?}")),
    }
}

/// Epoch the earliest still-queued job of `branch`'s latest run was created, or `None` when
/// nothing is queued (already picked up, or no run at all). Best-effort: this only sharpens
/// an existing stall judgement, so a forge hiccup here must not blind the rest of the poll —
/// the caller falls back to the coarser head-stall check instead.
pub fn read_queued_since(repo: &Repo, branch: &str) -> Option<u64> {
    if branch.is_empty() {
        return None;
    }
    let out = run(
        Command::new("bash").arg(&repo.forge).arg("queued-since").arg(&repo.path).arg(branch),
        "forge queued-since",
    )
    .ok()?;
    out.lines().next().unwrap_or("").trim().parse().ok()
}

pub fn read_pr_state(repo: &Repo, pr: &str) -> PrState {
    match forge(repo, "pr-state", pr, None, None).as_deref() {
        Ok("open") => PrState::Open,
        Ok("merged") => PrState::Merged,
        Ok("closed") => PrState::Closed,
        _ => PrState::Unknown,
    }
}

/// Is each member's tip on the base branch? Fetches first: an unfetched base is the stale
/// checkout that reads as "not landed" (law-closed-is-not-landed).
///
/// A `queue.local` repo's base (`local/main`) is a LOCAL branch already in this checkout, not
/// a remote-tracking one — `repo.local` is the structural fact from the repo-map's land
/// column, so this never falls back to guessing a remote off the ref's own spelling. Nothing
/// to fetch, and the ref is qualified to `refs/heads/...` so a repo that happens to have a
/// remote genuinely named `local` still resolves the LOCAL branch, never that remote's copy.
pub fn read_on_base(repo: &Repo, ms: &[Member]) -> Vec<(String, Option<bool>)> {
    if repo.local {
        return read_on_local_base(repo, ms);
    }
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
    // batch-job: git history or network operation, as long as the repository is large
    let fetched = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["fetch", "-q", remote])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ms.iter()
        .map(|m| {
            if !fetched || m.tip.is_empty() {
                return (m.id.clone(), None);
            }
            let st = spira_config::bounded::bounded("git")
                .arg("-C")
                .arg(&repo.path)
                .args(["merge-base", "--is-ancestor", &m.tip, &repo.base])
                .status();
            let v = match st.ok().and_then(|s| s.code()) {
                Some(0) => Some(true),
                Some(1) => Some(false),
                _ => None,
            };
            (m.id.clone(), v)
        })
        .collect()
}

fn read_on_local_base(repo: &Repo, ms: &[Member]) -> Vec<(String, Option<bool>)> {
    let base_ref = format!("refs/heads/{}", repo.base);
    ms.iter()
        .map(|m| {
            if m.tip.is_empty() {
                return (m.id.clone(), None);
            }
            let st = spira_config::bounded::bounded("git")
                .arg("-C")
                .arg(&repo.path)
                .args(["merge-base", "--is-ancestor", &m.tip, &base_ref])
                .status();
            let v = match st.ok().and_then(|s| s.code()) {
                Some(0) => Some(true),
                Some(1) => Some(false),
                _ => None,
            };
            (m.id.clone(), v)
        })
        .collect()
}

pub fn snapshot(env: &Env, repo: &Repo, prev: &RepoState, now: u64) -> Snapshot {
    let mut s = Snapshot { now, ..Default::default() };
    let qdir = env.queue_dir.join(&repo.name);

    match read_batch(&qdir) {
        Ok(b) => s.batch = b,
        Err(e) => s.errors.push(e),
    }
    match read_bisect(&qdir) {
        Ok(b) => s.bisect = b,
        Err(e) => s.errors.push(e),
    }
    match read_publish(&qdir) {
        Ok(p) => s.publish = p,
        Err(e) => s.errors.push(e),
    }
    let certified = match read_certified(env) {
        Ok(c) => c,
        Err(e) => {
            s.errors.push(e);
            vec![]
        }
    };
    if !s.errors.is_empty() {
        return s;
    }

    let mut ids: BTreeSet<String> = certified.iter().cloned().collect();
    for b in [s.batch.as_ref(), prev.batch()].into_iter().flatten() {
        ids.extend(b.members.iter().map(|m| m.id.clone()));
    }
    let express = match read_express(env) {
        Ok(e) => e,
        Err(e) => {
            s.errors.push(e);
            return s;
        }
    };
    match read_beads(env, &ids, &express) {
        Ok((beads, repos)) => {
            s.certified = certified
                .into_iter()
                .filter(|c| repos.get(c).map(|r| r == &repo.name).unwrap_or(false))
                .collect();
            s.beads = beads;
        }
        Err(e) => {
            s.errors.push(e);
            return s;
        }
    }

    if let Some(b) = &s.batch {
        match read_ci(repo, &b.pr, &b.branch) {
            Ok(c) => {
                if c == Ci::Pending {
                    s.queued_since = read_queued_since(repo, &b.branch);
                }
                s.ci = Some(c);
            }
            Err(e) => s.errors.push(e),
        }
    }

    // The previous batch is gone or replaced: ask the forge what became of it.
    if let Some(pb) = prev.batch() {
        if s.batch.as_ref().map(|b| b.pr != pb.pr).unwrap_or(true) {
            let state = read_pr_state(repo, &pb.pr);
            let on_base = if state == PrState::Merged { read_on_base(repo, &pb.members) } else { vec![] };
            s.outcome = Some(Outcome { pr: pb.pr.clone(), state, on_base });
        }
    }

    if let Some(p) = &s.publish {
        match read_ci(repo, &p.pr, &p.branch) {
            Ok(c) => s.publish_ci = Some(c),
            Err(e) => s.errors.push(e),
        }
    }
    if let Some(pp) = prev.publish() {
        if s.publish.as_ref().map(|p| p.pr != pp.pr).unwrap_or(true) {
            let state = read_pr_state(repo, &pp.pr);
            s.publish_outcome = Some(PublishOutcome { pr: pp.pr.clone(), state });
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn read_certified_needs_spira_lc_bin() {
        let env = Env { queue_dir: PathBuf::new(), db: None, lc_bin: None, lc_timeout: 5 };
        let err = read_certified(&env).unwrap_err();
        assert!(err.contains("SPIRA_LC_BIN unset"), "{err}");
    }
}
