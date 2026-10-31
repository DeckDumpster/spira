//! `unpublished <repo>` — a queue.local repo's unpublished range, measured against a forge tip
//! fetched now (the last fetched tip, marked `stale` with its age, when the forge does not answer). Prints `{"forge_tip","landing_tip","count","oldest_arrival"}`; `oldest_arrival` is
//! when the oldest unpublished commit reached the landing ref (its reflog), null at count 0.
//! A repo declared to publish nowhere answers `{"declared_unpublished":true}`: nothing to measure, not a blindness.
//! Exit 2, naming why, for anything it cannot measure — never a zero it did not read.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use spira_config::repos::{self, Registry};

use crate::git_evidence::{git_output, is_ancestor};

/// The server's bound on the verb: past the call's own deadline, so only a wedged run reaches it.
pub const DEADLINE: Duration = Duration::from_millis(4500);
const CALL_BUDGET: Duration = Duration::from_secs(4);
const FETCH_SECS: &str = "2";

pub fn run(args: &[String]) -> (i32, String) {
    let [name] = args else { return (2, "unpublished: usage: unpublished <repo>".into()) };
    let owned = name.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(measure(&owned));
    });
    match rx.recv_timeout(CALL_BUDGET) {
        Ok(Ok(v)) => (0, v.to_string()),
        Ok(Err(why)) => (2, format!("unpublished {name}: {why}")),
        Err(_) => (2, format!("unpublished {name}: not measured within {}s", CALL_BUDGET.as_secs())),
    }
}

fn measure(name: &str) -> Result<serde_json::Value, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = Registry::from_env(env.clone(), &home);
    let path = reg.root(name).ok_or("the repo map has no checkout for it")?;
    if reg.land(name) != "queue.local" {
        return Err("not a queue.local repo".into());
    }
    let repo = Path::new(&path);
    let base = repos::landref(&reg, name).ok_or("cannot resolve its landing ref")?;
    if repos::ref_remote(&base, Some(&path)).is_some() {
        return Err(format!("{base} is a remote-tracking ref, not a queue.local base"));
    }
    if repos::publishes_nowhere(&reg, name, &env) {
        return Ok(serde_json::json!({ "declared_unpublished": true }));
    }
    let (remote, branch) = repos::publish_forge(&reg, name, &env).ok_or("it has no resolvable publish target")?;
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let stale = match fetch(repo, &remote, &branch) {
        Ok(()) => None,
        Err(Fetch::TimedOut) => Some(tip_age(repo, &tracking)?),
        Err(Fetch::Failed(why)) => return Err(why),
    };
    let forge_tip = rev(repo, &tracking)?;
    let landing_tip = rev(repo, &base)?;
    let count: u64 = git_output(repo, &["rev-list", "--count", &format!("{forge_tip}..{landing_tip}")])
        .and_then(|s| s.trim().parse().ok())
        .ok_or("cannot count the range")?;
    let oldest_arrival = if count == 0 { None } else { Some(arrival(repo, &base, &forge_tip, &landing_tip)?) };
    let mut answer = serde_json::json!({ "forge_tip": forge_tip, "landing_tip": landing_tip, "count": count, "oldest_arrival": oldest_arrival });
    if let Some(age) = stale {
        answer["stale"] = true.into();
        answer["forge_tip_age_secs"] = age.into();
    }
    Ok(answer)
}

enum Fetch {
    TimedOut,
    Failed(String),
}

/// Seconds since the remote-tracking ref last moved; refused when no reflog says.
fn tip_age(repo: &Path, tracking: &str) -> Result<u64, String> {
    let last = git_output(repo, &["log", "-g", "-1", "--date=unix", "--format=%gd", tracking]).ok_or("the forge fetch timed out and no earlier fetch left a tip")?;
    let at: u64 = last.trim().rsplit_once('{').and_then(|(_, t)| t.strip_suffix('}')).and_then(|t| t.parse().ok()).ok_or("the forge fetch timed out and the last tip's age is unreadable")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    Ok(now.saturating_sub(at))
}

fn fetch(repo: &Path, remote: &str, branch: &str) -> Result<(), Fetch> {
    let out = spira_config::bounded::bounded_for("git", FETCH_SECS)
        .arg("-C")
        .arg(repo)
        .args(["fetch", "-q", remote, branch])
        .output()
        .map_err(|e| Fetch::Failed(format!("cannot run git: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    if out.status.code() == Some(124) {
        return Err(Fetch::TimedOut);
    }
    let why: String = String::from_utf8_lossy(&out.stderr).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
    Err(Fetch::Failed(format!("fetch of {remote}/{branch} failed ({}): {why}", out.status)))
}

fn rev(repo: &Path, name: &str) -> Result<String, String> {
    git_output(repo, &["rev-parse", "-q", "--verify", name]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or_else(|| format!("cannot resolve {name}"))
}

/// The first landing-ref update, oldest first, whose new tip already contains the range's oldest commit.
fn arrival(repo: &Path, base: &str, forge_tip: &str, landing_tip: &str) -> Result<u64, String> {
    let range = format!("{forge_tip}..{landing_tip}");
    let oldest = git_output(repo, &["rev-list", "--reverse", &range]).and_then(|s| s.lines().next().map(str::to_string)).ok_or("cannot find the oldest unpublished commit")?;
    let reflog = git_output(repo, &["log", "-g", "--date=unix", "--format=%H %gd", base]).ok_or("the landing ref has no readable reflog")?;
    let entries: Vec<(&str, u64)> = reflog
        .lines()
        .rev()
        .filter_map(|l| {
            let (tip, gd) = l.split_once(' ')?;
            let secs = gd.rsplit_once('{')?.1.strip_suffix('}')?.parse().ok()?;
            Some((tip, secs))
        })
        .collect();
    // The landing ref only advances, so containment is monotone along its reflog: bisect, one git call per step.
    let first = entries.partition_point(|(tip, _)| !is_ancestor(repo, &oldest, tip));
    entries.get(first).map(|(_, secs)| *secs).ok_or_else(|| "no reflog entry of the landing ref contains the oldest unpublished commit".into())
}
