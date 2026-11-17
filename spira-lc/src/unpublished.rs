//! `unpublished <repo>` — a queue.local repo's unpublished range, measured against the cached forge tip already in
//! the checkout and never waiting on the network: a tip a few seconds old or more is refreshed by a detached
//! background fetch, and one a minute old or more is answered `stale` with `forge_tip_age_secs`. Only a repo with no
//! tip at all fetches inline; a timeout there is answered stale from the tip's reflog. Prints
//! `{"forge_tip","landing_tip","count","oldest_arrival"}`; `oldest_arrival` is
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
/// The inline fetch of a repo that has no cached tip at all.
const FETCH_SECS: &str = "2";
/// The detached refresh runs off the call's budget, so it may wait longer than the inline fetch.
const BACKGROUND_FETCH_SECS: &str = "20";
/// A cached forge tip fetched within this long is answered from as it stands; older, a background fetch refreshes it for the next call.
const REFRESH_AFTER_SECS: u64 = 15;
/// A cached forge tip this old is answered `stale` with its age; it is never fetched inline.
const STALE_AFTER_SECS: u64 = 60;
/// A refresh started within this long is still in flight (its fetch is bounded by `BACKGROUND_FETCH_SECS`); no second one starts.
const REFRESH_MARK: &str = "spira-unpublished-refresh";
const REFRESH_IN_FLIGHT_SECS: u64 = 25;

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
    measure_repo(repo, &base, &remote, &branch)
}

fn measure_repo(repo: &Path, base: &str, remote: &str, branch: &str) -> Result<serde_json::Value, String> {
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let have_tip = rev(repo, &tracking).is_ok();
    let age = if have_tip { last_fetch_age(repo).or_else(|| tip_age(repo, &tracking).ok()) } else { None };
    let stale = match plan(age, have_tip) {
        Plan::Cached { age, refresh } => {
            if refresh {
                refresh_in_background(repo, remote, branch);
            }
            (age >= STALE_AFTER_SECS).then_some(age)
        }
        Plan::FetchNow => match fetch(repo, remote, branch) {
            Ok(()) => None,
            Err(Fetch::TimedOut) => Some(tip_age(repo, &tracking)?),
            Err(Fetch::Failed(why)) => return Err(why),
        },
    };
    let forge_tip = rev(repo, &tracking)?;
    let landing_tip = rev(repo, base)?;
    let (count, oldest_arrival) = range_facts(repo, base, &forge_tip, &landing_tip)?;
    let mut answer = serde_json::json!({ "forge_tip": forge_tip, "landing_tip": landing_tip, "count": count, "oldest_arrival": oldest_arrival });
    if let Some(age) = stale {
        answer["stale"] = true.into();
        answer["forge_tip_age_secs"] = age.into();
    }
    Ok(answer)
}

#[derive(Debug, PartialEq)]
enum Plan {
    FetchNow,
    Cached { age: u64, refresh: bool },
}

fn plan(tip_age: Option<u64>, have_tip: bool) -> Plan {
    match tip_age {
        Some(age) if have_tip => Plan::Cached { age, refresh: age >= REFRESH_AFTER_SECS },
        _ => Plan::FetchNow,
    }
}

/// Seconds since a fetch last completed in the checkout (FETCH_HEAD is rewritten even when no ref moved).
fn last_fetch_age(repo: &Path) -> Option<u64> {
    git_path_age(repo, "FETCH_HEAD")
}

/// Seconds since `rel` (a path inside the git dir) was last written, `None` when it never was.
fn git_path_age(repo: &Path, rel: &str) -> Option<u64> {
    let path = git_output(repo, &["rev-parse", "--git-path", rel])?;
    let modified = std::fs::metadata(repo.join(path.trim())).ok()?.modified().ok()?;
    Some(std::time::SystemTime::now().duration_since(modified).map_or(0, |d| d.as_secs()))
}

/// Claims the right to start a refresh: false while an earlier one is within its bound.
fn claim_refresh(repo: &Path) -> bool {
    if git_path_age(repo, REFRESH_MARK).is_some_and(|age| age < REFRESH_IN_FLIGHT_SECS) {
        return false;
    }
    let Some(path) = git_output(repo, &["rev-parse", "--git-path", REFRESH_MARK]) else { return false };
    std::fs::write(repo.join(path.trim()), "").is_ok()
}

fn refresh_in_background(repo: &Path, remote: &str, branch: &str) {
    if !claim_refresh(repo) {
        return;
    }
    let spawned = spira_config::bounded::bounded_for("git", BACKGROUND_FETCH_SECS)
        .arg("-C")
        .arg(repo)
        .args(["fetch", "-q", remote, branch])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
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

/// The unpublished count and when the landing ref first moved onto it: one `rev-list` of the range
/// and one reflog read, bounded by the range and the reflog, never by history.
fn range_facts(repo: &Path, base: &str, forge_tip: &str, landing_tip: &str) -> Result<(u64, Option<u64>), String> {
    let range = git_output(repo, &["rev-list", &format!("{forge_tip}..{landing_tip}")]).ok_or("cannot list the unpublished range")?;
    let count = range.split_whitespace().count() as u64;
    if count == 0 {
        return Ok((0, None));
    }
    let reflog = git_output(repo, &["log", "-g", "--date=unix", "--format=%H %gd", base]).ok_or("the landing ref has no readable reflog")?;
    Ok((count, Some(arrival_in(&range, &reflog)?)))
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

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.logAllRefUpdates=always"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn landed(history: usize) -> (u64, Option<u64>) {
        let scratch = testkit::TempDir::new("unpublished-range");
        let dir = scratch.path();
        git(dir, &["init", "-q", "-b", "main"]);
        for _ in 0..history {
            git(dir, &["commit", "-q", "--allow-empty", "-m", "old"]);
        }
        let forge = git(dir, &["rev-parse", "HEAD"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let tip = git(dir, &["rev-parse", "HEAD"]);
        range_facts(dir, "main", &forge, &tip).unwrap()
    }

    #[test]
    fn a_long_history_before_the_published_commit_measures_the_same_range_as_a_short_one() {
        let (short, long) = (landed(2), landed(20));
        assert_eq!(short.0, 2);
        assert_eq!(long.0, 2);
        assert!(short.1.is_some() && long.1.is_some());
    }

    #[test]
    fn a_cached_forge_tip_is_always_answered_from_and_refreshed_in_the_background_once_a_few_seconds_old() {
        assert_eq!(plan(Some(1), true), Plan::Cached { age: 1, refresh: false });
        assert_eq!(plan(Some(14), true), Plan::Cached { age: 14, refresh: false });
        assert_eq!(plan(Some(30), true), Plan::Cached { age: 30, refresh: true });
        assert_eq!(plan(Some(60), true), Plan::Cached { age: 60, refresh: true });
        assert_eq!(plan(Some(900), true), Plan::Cached { age: 900, refresh: true });
        assert_eq!(plan(None, true), Plan::FetchNow);
        assert_eq!(plan(Some(1), false), Plan::FetchNow);
    }

    #[test]
    fn fifty_unpublished_commits_are_counted_from_one_range_list_and_one_reflog_read() {
        let scratch = testkit::TempDir::new("unpublished-fifty");
        let dir = scratch.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "published"]);
        let forge = git(dir, &["rev-parse", "HEAD"]);
        for i in 0..50 {
            git(dir, &["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
        }
        let tip = git(dir, &["rev-parse", "HEAD"]);
        git(dir, &["update-ref", "refs/remotes/origin/main", &forge]);
        std::fs::write(dir.join(".git/FETCH_HEAD"), "").unwrap();
        let age = last_fetch_age(dir).expect("FETCH_HEAD is read");
        assert!(age < REFRESH_AFTER_SECS, "{age}");
        let (count, arrival) = range_facts(dir, "main", &forge, &tip).unwrap();
        assert_eq!(count, 50);
        assert!(arrival.is_some());
    }

    #[test]
    fn a_second_refresh_within_the_first_ones_bound_is_not_started() {
        let scratch = testkit::TempDir::new("unpublished-claim");
        let dir = scratch.path();
        git(dir, &["init", "-q", "-b", "main"]);
        assert!(claim_refresh(dir));
        assert!(!claim_refresh(dir));
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

    // law-a-unit-test-calls-no-binary: the freshness under test is git's own FETCH_HEAD write.
    fn forge_and_clone(scratch: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let (forge, clone) = (scratch.join("forge.git"), scratch.join("clone"));
        std::fs::create_dir_all(&forge).unwrap();
        git(&forge, &["init", "-q", "--bare", "-b", "main"]);
        let seed = scratch.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q", "-b", "main"]);
        git(&seed, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&seed, &["push", "-q", forge.to_str().unwrap(), "main"]);
        git(scratch, &["clone", "-q", forge.to_str().unwrap(), clone.to_str().unwrap()]);
        git(&clone, &["fetch", "-q", "origin", "main"]);
        (forge, clone)
    }

    fn age_fetch_head(clone: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(clone.join(".git/FETCH_HEAD")).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }

    #[test]
    fn a_hung_forge_is_not_fetched_inline_and_the_cached_tip_is_answered_with_its_age() {
        let scratch = testkit::TempDir::new("unpublished-hung");
        let (_forge, clone) = forge_and_clone(scratch.path());
        let cached = git(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        let fetched = scratch.path().join("fetched");
        git(&clone, &["config", "remote.origin.uploadpack", &format!("touch {}; sleep 30", fetched.display())]);
        age_fetch_head(&clone, 120);
        assert!(claim_refresh(&clone));
        let answer = measure_repo(&clone, "main", "origin", "main").unwrap();
        assert_eq!(answer["stale"], true, "{answer}");
        assert!(answer["forge_tip_age_secs"].as_u64().unwrap() >= 120, "{answer}");
        assert_eq!(answer["forge_tip"], cached.as_str());
        assert!(!fetched.exists(), "the forge was fetched inline");
    }

    #[test]
    fn a_call_on_an_old_tip_answers_stale_from_the_cache_and_claims_the_refresh() {
        let scratch = testkit::TempDir::new("unpublished-claims");
        let (_forge, clone) = forge_and_clone(scratch.path());
        let cached = git(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        age_fetch_head(&clone, 120);
        let answer = measure_repo(&clone, "main", "origin", "main").unwrap();
        assert_eq!(answer["stale"], true, "{answer}");
        assert_eq!(answer["forge_tip"], cached.as_str());
        assert!(!claim_refresh(&clone));
    }

    #[test]
    fn a_tip_with_no_fetch_head_is_aged_from_its_reflog() {
        let scratch = testkit::TempDir::new("unpublished-nohead");
        let (_forge, clone) = forge_and_clone(scratch.path());
        let cached = git(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        git(&clone, &["update-ref", "-d", "refs/remotes/origin/main"]);
        git(&clone, &["update-ref", "--create-reflog", "-m", "fetch", "refs/remotes/origin/main", &cached]);
        let fetched = scratch.path().join("fetched");
        git(&clone, &["config", "remote.origin.uploadpack", &format!("touch {}; sleep 30", fetched.display())]);
        std::fs::remove_file(clone.join(".git/FETCH_HEAD")).unwrap();
        assert!(claim_refresh(&clone));
        let answer = measure_repo(&clone, "main", "origin", "main").unwrap();
        assert!(answer.get("stale").is_none(), "{answer}");
        assert!(!fetched.exists(), "a tip with a reflog was fetched inline");
    }

    #[test]
    fn an_unreachable_forge_answers_from_the_cached_tip_named_stale() {
        let scratch = testkit::TempDir::new("unpublished-unreachable");
        let (_forge, clone) = forge_and_clone(scratch.path());
        let cached = git(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        git(&clone, &["remote", "set-url", "origin", scratch.path().join("nonexistent").to_str().unwrap()]);
        age_fetch_head(&clone, 120);
        let answer = measure_repo(&clone, "main", "origin", "main").unwrap();
        assert_eq!(answer["stale"], true);
        assert!(answer["forge_tip_age_secs"].as_u64().unwrap() >= 120, "{answer}");
        assert_eq!(answer["forge_tip"], cached.as_str());
    }

    #[test]
    fn a_measurement_that_fails_exits_nonzero_naming_why() {
        assert_eq!(run(&["no-such-repo-sp-hnsjdu".into()]).0, 2);
        assert_eq!(run(&[]).0, 2);
    }
}
