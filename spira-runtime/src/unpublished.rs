//! How far a queue.local repo's forge trails its landing ref, measured from the checkout the
//! backlog snapshot has just refreshed and recorded for lc-serve (`git-fact put unpublished`).
//! `read_at` is when the forge tip was last known current, so a failed refresh ages the record.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use spira_config::repos::{self, Registry};

use crate::tasks::a::queue_step::edge::git;

/// Whether the landing ref and the forge's remote-tracking ref are one commit already: no fetch can
/// make them agree, and the first one that moves the forge past it is seen after the landing ref moves.
pub(crate) fn published(repo: &Path, base: &str, remote: &str, branch: &str) -> bool {
    spira_config::git_refs::same_tip(repo, base, &format!("refs/remotes/{remote}/{branch}")).is_some()
}

const GIT_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
pub enum Measurement {
    Declared,
    Measured(Measured),
}

#[derive(Debug, PartialEq)]
pub struct Measured {
    pub landing_tip: String,
    pub forge_tip: String,
    pub count: u64,
    pub oldest_arrival: Option<u64>,
    pub read_at: u64,
}

impl Measurement {
    /// The body of `git-fact put unpublished`.
    pub fn record(&self, repo: &str) -> String {
        match self {
            Measurement::Declared => serde_json::json!({ "repo": repo, "declared_unpublished": true }),
            Measurement::Measured(m) => serde_json::json!({
                "repo": repo, "landing_tip": m.landing_tip, "forge_tip": m.forge_tip, "count": m.count,
                "oldest_arrival": m.oldest_arrival, "read_at": m.read_at,
            }),
        }
        .to_string()
    }
}

pub fn measure(name: &str, now: u64) -> Result<Measurement, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = Registry::from_env(env.clone(), &home);
    let path = reg.root(name).ok_or("the repo map has no checkout for it")?;
    if reg.land(name) != "queue.local" {
        return Err("not a queue.local repo".into());
    }
    let base = repos::landref(&reg, name).ok_or("cannot resolve its landing ref")?;
    if repos::ref_remote(&base, Some(&path)).is_some() {
        return Err(format!("{base} is a remote-tracking ref, not a queue.local base"));
    }
    if repos::publishes_nowhere(&reg, name, &env) {
        return Ok(Measurement::Declared);
    }
    let (remote, branch) = repos::publish_forge(&reg, name, &env).ok_or("it has no resolvable publish target")?;
    measure_repo(Path::new(&path), &base, &remote, &branch, now).map(Measurement::Measured)
}

pub(crate) fn measure_repo(repo: &Path, base: &str, remote: &str, branch: &str, now: u64) -> Result<Measured, String> {
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let forge_tip = rev(repo, &tracking).map_err(|e| format!("{e}: the forge has not been fetched"))?;
    let age = last_fetch_age(repo, now).or_else(|| tip_age(repo, &tracking, now)).ok_or("the forge tip's age is unknown: no fetch left a FETCH_HEAD or a reflog")?;
    let landing_tip = rev(repo, base)?;
    let (count, oldest_arrival) = if forge_tip == landing_tip { (0, None) } else { range_facts(repo, base, &forge_tip, &landing_tip)? };
    Ok(Measured { landing_tip, forge_tip, count, oldest_arrival, read_at: now.saturating_sub(age) })
}

/// Seconds since a fetch last completed in the checkout (FETCH_HEAD is rewritten even when no ref moved).
fn last_fetch_age(repo: &Path, now: u64) -> Option<u64> {
    let (gitdir, _) = spira_config::git_refs::git_dirs(repo)?;
    let modified = std::fs::metadata(gitdir.join("FETCH_HEAD")).ok()?.modified().ok()?;
    let at = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(at))
}

/// Seconds since the remote-tracking ref last moved, from its reflog.
fn tip_age(repo: &Path, tracking: &str, now: u64) -> Option<u64> {
    let last = git(repo, &["log", "-g", "-1", "--date=unix", "--format=%gd", tracking], GIT_DEADLINE).ok()?;
    let at: u64 = last.trim().rsplit_once('{').and_then(|(_, t)| t.strip_suffix('}')).and_then(|t| t.parse().ok())?;
    Some(now.saturating_sub(at))
}

fn rev(repo: &Path, name: &str) -> Result<String, String> {
    if let Some(sha) = spira_config::git_refs::sha_of(repo, name) {
        return Ok(sha);
    }
    git(repo, &["rev-parse", "-q", "--verify", name], GIT_DEADLINE).ok().filter(|s| !s.is_empty()).ok_or_else(|| format!("cannot resolve {name}"))
}

/// The unpublished count and when the landing ref first moved onto it: one `rev-list` of the range
/// and one reflog read, bounded by the range and the reflog, never by history.
fn range_facts(repo: &Path, base: &str, forge_tip: &str, landing_tip: &str) -> Result<(u64, Option<u64>), String> {
    let range = git(repo, &["rev-list", &format!("{forge_tip}..{landing_tip}")], GIT_DEADLINE).map_err(|e| format!("cannot list the unpublished range: {e}"))?;
    let count = range.split_whitespace().count() as u64;
    if count == 0 {
        return Ok((0, None));
    }
    let reflog = git(repo, &["log", "-g", "--date=unix", "--format=%H %gd", base], GIT_DEADLINE).map_err(|e| format!("the landing ref has no readable reflog: {e}"))?;
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

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.logAllRefUpdates=always", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn epoch() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    #[test_id(tnnew4k)]
    #[test]
    fn the_arrival_is_the_oldest_reflog_update_landing_on_the_range_whatever_the_history_before_it() {
        let reflog = "ddd x@{400}\nccc x@{300}\nbbb x@{200}\naaa x@{100}\n";
        assert_eq!(arrival_in("ddd\nccc\nbbb\n", reflog), Ok(200));
        let deep: String = (0..5000).map(|i| format!("old{i} x@{{{i}}}\n")).collect();
        assert_eq!(arrival_in("ddd\nccc\nbbb\n", &format!("{reflog}{deep}")), Ok(200));
    }

    #[test_id(tgfmgnr)]
    #[test]
    fn an_arrival_no_reflog_entry_records_is_refused() {
        assert!(arrival_in("zzz", "aaa x@{100}\n").is_err());
    }

    /// A checkout whose `origin/main` sits at the first commit and whose `main` is `ahead` commits past it.
    fn clone_ahead(scratch: &Path, ahead: usize) -> std::path::PathBuf {
        let (forge, clone) = (scratch.join("forge.git"), scratch.join("clone"));
        std::fs::create_dir_all(&forge).unwrap();
        run(&forge, &["init", "-q", "--bare", "-b", "main"]);
        std::fs::create_dir_all(&clone).unwrap();
        run(&clone, &["init", "-q", "-b", "main"]);
        run(&clone, &["commit", "-q", "--allow-empty", "-m", "published"]);
        run(&clone, &["push", "-q", forge.to_str().unwrap(), "main"]);
        run(&clone, &["remote", "add", "origin", forge.to_str().unwrap()]);
        run(&clone, &["fetch", "-q", "origin", "main"]);
        for i in 0..ahead {
            run(&clone, &["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
        }
        clone
    }

    #[test_id(tcqfw77)]
    #[test]
    fn fifty_unpublished_commits_are_counted_with_their_arrival_and_the_tips_they_were_read_at() {
        let scratch = testkit::TempDir::new("runtime-unpublished-fifty");
        let clone = clone_ahead(scratch.path(), 50);
        let m = measure_repo(&clone, "main", "origin", "main", epoch()).unwrap();
        assert_eq!(m.count, 50);
        assert!(m.oldest_arrival.is_some());
        assert_eq!(m.landing_tip, run(&clone, &["rev-parse", "main"]));
        assert_eq!(m.forge_tip, run(&clone, &["rev-parse", "refs/remotes/origin/main"]));
    }

    #[test_id(trmeaje)]
    #[test]
    fn a_long_history_before_the_published_commit_measures_the_same_range_as_a_short_one() {
        let count = |history: usize| {
            let scratch = testkit::TempDir::new("runtime-unpublished-history");
            let clone = clone_ahead(scratch.path(), 0);
            for _ in 0..history {
                run(&clone, &["commit", "-q", "--allow-empty", "-m", "old"]);
            }
            run(&clone, &["push", "-q", "origin", "main"]);
            run(&clone, &["fetch", "-q", "origin", "main"]);
            run(&clone, &["commit", "-q", "--allow-empty", "-m", "one"]);
            run(&clone, &["commit", "-q", "--allow-empty", "-m", "two"]);
            measure_repo(&clone, "main", "origin", "main", epoch()).unwrap().count
        };
        assert_eq!((count(2), count(20)), (2, 2));
    }

    #[test_id(tryf20s)]
    #[test]
    fn a_fetch_that_completed_a_while_ago_ages_the_record_by_it() {
        let scratch = testkit::TempDir::new("runtime-unpublished-aged");
        let clone = clone_ahead(scratch.path(), 2);
        let f = std::fs::OpenOptions::new().write(true).open(clone.join(".git/FETCH_HEAD")).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(120)).unwrap();
        let now = epoch();
        let m = measure_repo(&clone, "main", "origin", "main", now).unwrap();
        assert!((118..=122).contains(&(now - m.read_at)), "{}", now - m.read_at);
        assert_eq!(m.count, 2);
    }

    #[test_id(ty7z6jm)]
    #[test]
    fn a_tip_with_no_fetch_head_is_aged_from_its_reflog_and_one_with_neither_is_refused() {
        let scratch = testkit::TempDir::new("runtime-unpublished-reflog");
        let clone = clone_ahead(scratch.path(), 1);
        std::fs::remove_file(clone.join(".git/FETCH_HEAD")).unwrap();
        let now = epoch() + 100;
        let m = measure_repo(&clone, "main", "origin", "main", now).unwrap();
        assert!(m.read_at < now - 90, "{}", now - m.read_at);
        let tip = run(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        std::fs::remove_dir_all(clone.join(".git/logs/refs/remotes")).unwrap();
        let why = measure_repo(&clone, "main", "origin", "main", now).unwrap_err();
        assert!(why.contains("age is unknown"), "{why} ({tip})");
    }

    #[test_id(t3qng4m)]
    #[test]
    fn a_checkout_never_fetched_is_refused_naming_that() {
        let scratch = testkit::TempDir::new("runtime-unpublished-unfetched");
        let clone = clone_ahead(scratch.path(), 1);
        run(&clone, &["update-ref", "-d", "refs/remotes/origin/main"]);
        let why = measure_repo(&clone, "main", "origin", "main", epoch()).unwrap_err();
        assert!(why.contains("has not been fetched"), "{why}");
    }

    #[test_id(tmnbdi5)]
    #[test]
    fn the_record_body_names_the_repo_and_carries_the_tips_or_the_declaration() {
        let m = Measurement::Measured(Measured { landing_tip: "b".into(), forge_tip: "a".into(), count: 2, oldest_arrival: None, read_at: 9 });
        let v: serde_json::Value = serde_json::from_str(&m.record("r")).unwrap();
        assert_eq!((v["repo"].clone(), v["count"].clone(), v["oldest_arrival"].clone(), v["read_at"].clone()), ("r".into(), 2.into(), serde_json::Value::Null, 9.into()));
        let d: serde_json::Value = serde_json::from_str(&Measurement::Declared.record("r")).unwrap();
        assert_eq!(d["declared_unpublished"], true);
    }
}
