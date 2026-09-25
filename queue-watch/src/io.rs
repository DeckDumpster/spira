//! The impure half: read one repo's queue state, the bead store and the forge into a
//! `Snapshot`. Every read that fails becomes an entry in `Snapshot::errors` — never an empty
//! value — so the core can say "blind" rather than "quiet".

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::{Batch, Bead, Ci, Member, Outcome, PrState, RepoState, Snapshot};

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
    pub forge: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Env {
    pub queue_dir: PathBuf,
    pub landstate: PathBuf,
    pub db: Option<PathBuf>,
    pub bd: String,
    pub express_label: String,
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
        members: members(kv.get("members").map(String::as_str).unwrap_or("")),
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
    let mtime = fs::metadata(&p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(Some((members(first).into_iter().map(|m| m.id).collect(), mtime)))
}

/// Every bead whose landstate record reads CERTIFIED. Records are shared across repos; the
/// caller narrows by the bead's `repo:` label.
pub fn read_certified(landstate: &Path) -> Result<Vec<String>, String> {
    let rd = fs::read_dir(landstate).map_err(|e| format!("{}: {e}", landstate.display()))?;
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if !name.starts_with("sp-") || name.contains(".gate-key") || name.contains(".tmp") {
            continue;
        }
        // A bead id may itself contain a dot (sp-s088v.5); a sidecar is `<id>.<word>`.
        if let Some((_, ext)) = name.rsplit_once('.') {
            if ext.chars().any(|c| c.is_ascii_alphabetic()) && ext.len() > 2 {
                continue;
            }
        }
        if let Ok(t) = fs::read_to_string(ent.path()) {
            if t.split_whitespace().next() == Some("CERTIFIED") {
                out.push(name);
            }
        }
    }
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

pub fn read_beads(env: &Env, ids: &BTreeSet<String>) -> Result<(BTreeMap<String, Bead>, BTreeMap<String, String>), String> {
    let mut beads = BTreeMap::new();
    let mut repos = BTreeMap::new();
    if ids.is_empty() {
        return Ok((beads, repos));
    }
    let mut cmd = Command::new(&env.bd);
    if let Some(db) = &env.db {
        cmd.current_dir(db);
    }
    cmd.arg("show").arg("--json").args(ids);
    let text = run(&mut cmd, "bd show")?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("bd show: unparsed output: {e}"))?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
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
                express: labels.contains(&env.express_label.as_str()),
            },
        );
    }
    Ok((beads, repos))
}

fn forge(repo: &Repo, sub: &str, pr: &str) -> Result<String, String> {
    let out = run(
        Command::new("bash").arg(&repo.forge).arg(sub).arg(&repo.path).arg(pr),
        &format!("forge {sub} {pr}"),
    )?;
    Ok(out.lines().next().unwrap_or("").trim().to_string())
}

pub fn read_ci(repo: &Repo, pr: &str) -> Result<Ci, String> {
    match forge(repo, "check-status", pr)?.as_str() {
        "pending" => Ok(Ci::Pending),
        "green" => Ok(Ci::Green),
        "red" => Ok(Ci::Red),
        k @ ("harness_fault" | "provision_fault") => Ok(Ci::Fault(k.to_string())),
        other => Err(format!("forge check-status {pr}: unrecognised answer {other:?}")),
    }
}

pub fn read_pr_state(repo: &Repo, pr: &str) -> PrState {
    match forge(repo, "pr-state", pr).as_deref() {
        Ok("open") => PrState::Open,
        Ok("merged") => PrState::Merged,
        Ok("closed") => PrState::Closed,
        _ => PrState::Unknown,
    }
}

/// Is each member's tip on the base branch? Fetches first: an unfetched base is the stale
/// checkout that reads as "not landed" (law-closed-is-not-landed).
pub fn read_on_base(repo: &Repo, ms: &[Member]) -> Vec<(String, Option<bool>)> {
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
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
            let st = Command::new("git")
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
    let certified = match read_certified(&env.landstate) {
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
    match read_beads(env, &ids) {
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
        match read_ci(repo, &b.pr) {
            Ok(c) => s.ci = Some(c),
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
    s
}
