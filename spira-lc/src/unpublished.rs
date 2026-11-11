//! `unpublished <repo>` — a queue.local repo's unpublished range, measured against a forge tip
//! fetched now (the last fetched tip, marked `stale` with its age, when the forge does not answer). Prints `{"forge_tip","landing_tip","count","oldest_arrival"}`; `oldest_arrival` is
//! when the oldest unpublished commit reached the landing ref (its reflog), null at count 0.
//! A repo declared to publish nowhere answers `{"declared_unpublished":true}`: nothing to measure, not a blindness.
//! Exit 2, naming why, for anything it cannot measure — never a zero it did not read.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use spira_config::repos::{self, Registry};

use crate::git_evidence::git_output;

/// The server's bound on the verb: past the call's own deadline, so only a wedged run reaches it.
pub const DEADLINE: Duration = Duration::from_millis(4500);
const CALL_BUDGET: Duration = Duration::from_secs(4);
const FETCH_SECS: &str = "2";

pub fn run(args: &[String]) -> (i32, String) {
    let [name] = args else { return (2, "unpublished: usage: unpublished <repo>".into()) };
    let owned = name.clone();
    answer_within(name, CALL_BUDGET, move || measure(&owned))
}

fn answer_within(name: &str, budget: Duration, f: impl FnOnce() -> Result<serde_json::Value, String> + Send + 'static) -> (i32, String) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(budget) {
        Ok(Ok(v)) => (0, v.to_string()),
        Ok(Err(why)) => (2, format!("unpublished {name}: {why}")),
        Err(_) => (2, format!("unpublished {name}: not measured within {}s", budget.as_secs())),
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
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    tip_age_of(git_output(repo, &["log", "-g", "-1", "--date=unix", "--format=%gd", tracking]), now)
}

/// `last` is the tracking ref's newest reflog entry (`%gd`, unix date), absent when no fetch
/// ever moved it: a repo never fetched has no tip to be stale against, so it is refused.
fn tip_age_of(last: Option<String>, now: u64) -> Result<u64, String> {
    let last = last.ok_or("the forge fetch timed out and no earlier fetch left a tip")?;
    let at: u64 = last.trim().rsplit_once('{').and_then(|(_, t)| t.strip_suffix('}')).and_then(|t| t.parse().ok()).ok_or("the forge fetch timed out and the last tip's age is unreadable")?;
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

/// When the landing ref first moved onto the unpublished range: two git calls, both bounded by the
/// range and the reflog, never by history.
fn arrival(repo: &Path, base: &str, forge_tip: &str, landing_tip: &str) -> Result<u64, String> {
    let range = git_output(repo, &["rev-list", &format!("{forge_tip}..{landing_tip}")]).ok_or("cannot list the unpublished range")?;
    let reflog = git_output(repo, &["log", "-g", "--date=unix", "--format=%H %gd", base]).ok_or("the landing ref has no readable reflog")?;
    arrival_in(&range, &reflog)
}

/// The oldest reflog update (`<tip> <ref>@{<unix>}` per line, newest first) whose tip is a commit of the range.
fn arrival_in(range: &str, reflog: &str) -> Result<u64, String> {
    let range: std::collections::HashSet<&str> = range.split_whitespace().collect();
    reflog
        .lines()
        .rev()
        .filter_map(|l| {
            let (tip, gd) = l.split_once(' ')?;
            let secs = gd.rsplit_once('{')?.1.strip_suffix('}')?.parse().ok()?;
            range.contains(tip).then_some(secs)
        })
        .next()
        .ok_or_else(|| "no reflog entry of the landing ref lands on the unpublished range".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_fetch_of_a_repo_never_fetched_is_refused_naming_that_cause() {
        assert_eq!(tip_age_of(None, 1_000), Err("the forge fetch timed out and no earlier fetch left a tip".into()));
    }

    #[test]
    fn a_timed_out_fetch_with_an_earlier_tip_is_measured_against_it_with_its_age() {
        assert_eq!(tip_age_of(Some("refs/remotes/origin/main@{900}\n".into()), 1_000), Ok(100));
        assert_eq!(tip_age_of(Some("garbage".into()), 1_000), Err("the forge fetch timed out and the last tip's age is unreadable".into()));
    }

    #[test]
    fn the_arrival_is_the_oldest_reflog_update_landing_on_the_range_whatever_the_history_before_it() {
        let reflog = "ddd x@{400}\nccc x@{300}\nbbb x@{200}\naaa x@{100}\n";
        assert_eq!(arrival_in("ddd\nccc\nbbb\n", reflog), Ok(200));
        let deep: String = (0..5000).map(|i| format!("old{i} x@{{{i}}}\n")).collect();
        assert_eq!(arrival_in("ddd\nccc\nbbb\n", &format!("{reflog}{deep}")), Ok(200));
    }

    #[test]
    fn an_arrival_no_reflog_entry_records_is_refused() {
        assert!(arrival_in("zzz", "aaa x@{100}\n").is_err());
    }

    #[test]
    fn a_measurement_that_overruns_its_budget_exits_nonzero() {
        let (hold, wedged) = std::sync::mpsc::channel::<()>();
        let (code, out) = answer_within("r", Duration::from_millis(50), move || {
            let _ = wedged.recv();
            Ok(serde_json::json!({}))
        });
        drop(hold);
        assert_eq!(code, 2);
        assert!(out.contains("not measured within"), "{out}");
    }

    #[test]
    fn a_measurement_that_fails_exits_nonzero_naming_why() {
        assert_eq!(run(&["no-such-repo-sp-hnsjdu".into()]).0, 2);
        assert_eq!(run(&[]).0, 2);
    }
}
