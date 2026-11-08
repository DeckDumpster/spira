//! `--deploy-fault-check` — land-local leaves `<queue dir>/<repo>/deploy-fault`
//! (`<head> <why>`) when the release deploy of a landing failed, and removes it on a
//! successful activation. While it stands the landed fixes are not in force: alarm, and
//! retry at most once per interval. A release of the head that was built and accepted is
//! activated from its own `bin/release` (sp-6ugk85: an accepted release sat unactivated while
//! this check only rebuilt it); one not built yet is rebuilt.

use crate::incident::{self, Finding};
use crate::log::log;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Cfg {
    pub release_bin: String,
    pub repo: String,
    pub retry_secs: i64,
    pub build_timeout_secs: u64,
    /// `$SPIRA_RELEASES`: where an accepted release of the head is looked for. `None` when the
    /// box names none, and the retry only rebuilds.
    pub releases: Option<PathBuf>,
}

pub fn parse(marker: &str) -> Option<(&str, &str)> {
    let line = marker.lines().next()?.trim();
    let (head, why) = line.split_once(' ').unwrap_or((line, ""));
    (!head.is_empty() && head.bytes().all(|b| b.is_ascii_hexdigit())).then_some((head, why.trim()))
}

pub fn retry_due(last: Option<i64>, now: i64, retry_secs: i64) -> bool {
    last.is_none_or(|t| now - t >= retry_secs)
}

/// The retry stamp: `<head> <epoch>`. A stamp for another head (an earlier fault) is no
/// attempt at this one, so the new head's first retry is due at once; neither is a bare epoch,
/// the format before the stamp named its head, which cannot say whose attempt it was.
fn last_attempt(stamp: &str, head: &str) -> Option<i64> {
    let mut w = stamp.split_whitespace();
    match (w.next(), w.next()) {
        (Some(h), Some(t)) if h == head => t.parse().ok(),
        _ => None,
    }
}

fn retry_build(cfg: &Cfg, head: &str) -> String {
    let out = Command::new("timeout")
        .arg(cfg.build_timeout_secs.to_string())
        .arg(&cfg.release_bin)
        .args(["build", head, "--repo", &cfg.repo])
        .output();
    match out {
        Ok(o) if o.status.success() => format!("retry `release build {head}` succeeded"),
        Ok(o) => format!(
            "retry `release build {head}` failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")
        ),
        Err(e) => format!("retry could not run {}: {e}", cfg.release_bin),
    }
}

/// How a retry ended.
enum Retry {
    /// The head's release is active: the marker is obsolete.
    Activated(String),
    /// Anything else, as the alarm's "last attempt" text.
    Not(String),
}

/// `git merge-base --is-ancestor a b` in `repo`; `Err` when git cannot say.
fn is_ancestor(repo: &str, a: &str, b: &str) -> Result<bool, String> {
    let o = Command::new("timeout").args(["5", "git", "-C", repo, "merge-base", "--is-ancestor", a, b]).output().map_err(|e| format!("cannot run git: {e}"))?;
    match o.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("git merge-base --is-ancestor {a} {b} in {repo:?}: {}", String::from_utf8_lossy(&o.stderr).trim())),
    }
}

/// What `release status --json` says: the release in force, and the head's machine state.
fn release_status(cfg: &Cfg, head: &str) -> Result<(String, String), String> {
    let o = Command::new("timeout").arg("5").arg(&cfg.release_bin).args(["status", "--json"]).output().map_err(|e| format!("cannot run {}: {e}", cfg.release_bin))?;
    if !o.status.success() {
        return Err(format!("release status --json exited {}: {}", o.status, String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")));
    }
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).map_err(|e| format!("release status --json: {e}"))?;
    let current = v["current"].as_str().unwrap_or("").to_string();
    let state = v["releases"].as_array().and_then(|rs| rs.iter().find(|r| r["sha"] == head)).and_then(|r| r["state"].as_str()).unwrap_or("unrecorded").to_string();
    Ok((current, state))
}

/// Activate the head's built release from its own binary, under the activation lock land-local
/// takes (`<queue dir>/<repo>/lock.activate`), and only when the release machine has it
/// accepted (or published: an activation started and did not finish) and the head is newer
/// than `current`: a hand activation of a later release does not clear the marker, and
/// activating the head over it would roll back.
fn retry_activate(cfg: &Cfg, releases: &Path, repo_dir: &Path, head: &str) -> Retry {
    let (current, state) = match release_status(cfg, head) {
        Ok(s) => s,
        Err(e) => return Retry::Not(format!("not activated: {e}")),
    };
    if current == head {
        return Retry::Activated(format!("{head} is already current"));
    }
    if !matches!(state.as_str(), "accepted" | "published") {
        return Retry::Not(format!("not activated: release {head} is {state}, not accepted"));
    }
    if !current.is_empty() {
        match is_ancestor(&cfg.repo, &current, head) {
            Ok(true) => {}
            Ok(false) => return Retry::Activated(format!("current {current} is not an ancestor of {head}; a later release is in force")),
            Err(e) => return Retry::Not(format!("not activated: cannot tell whether {head} is newer than current {current}: {e}")),
        }
    }
    let lock = match std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(repo_dir.join("lock.activate")) {
        Ok(f) => f,
        Err(e) => return Retry::Not(format!("not activated: cannot open the activation lock: {e}")),
    };
    if lock.try_lock().is_err() {
        return Retry::Not("not activated: another activation holds the lock; retrying next pass".into());
    }
    let bin = releases.join(head).join("bin/release");
    // batch-job: release activation restarts units and drains oneshots, bounded by timeout(1)
    let out = Command::new("timeout").arg(cfg.build_timeout_secs.to_string()).arg(&bin).args(["activate", head, "--repo", &cfg.repo]).output();
    match out {
        Ok(o) if o.status.success() => Retry::Activated(format!("retry `release activate {head}` succeeded")),
        Ok(o) => Retry::Not(format!("retry `release activate {head}` failed ({}): {}", o.status, String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or(""))),
        Err(e) => Retry::Not(format!("retry could not run {}: {e}", bin.display())),
    }
}

pub fn run(now: i64, queue_dir: &Path, run_dir: &Path, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    let Ok(entries) = std::fs::read_dir(queue_dir) else { return };
    for e in entries.flatten() {
        let marker = e.path().join("deploy-fault");
        let Ok(text) = std::fs::read_to_string(&marker) else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((head, why)) = parse(&text) else {
            log(&format!("watchtower: deploy-fault-check: unreadable marker {}", marker.display()));
            continue;
        };
        let stamp = run_dir.join(format!("deploy-fault-retry.{name}"));
        let last = std::fs::read_to_string(&stamp).ok().and_then(|s| last_attempt(&s, head));
        let built = cfg.releases.as_deref().filter(|r| r.join(head).join("bin/release").is_file());
        let retried = if retry_due(last, now, cfg.retry_secs) {
            let _ = std::fs::write(&stamp, format!("{head} {now}"));
            match built {
                Some(releases) => retry_activate(cfg, releases, &e.path(), head),
                None => Retry::Not(retry_build(cfg, head)),
            }
        } else {
            Retry::Not(format!("retry not yet due (last attempt {}s ago, every {}s)", now - last.unwrap_or(now), cfg.retry_secs))
        };
        let retried = match retried {
            Retry::Activated(how) => {
                let _ = std::fs::remove_file(&marker);
                let _ = std::fs::remove_file(&stamp);
                log(&format!("watchtower: deploy-fault-check: {name} {head} — {how}; marker cleared"));
                continue;
            }
            Retry::Not(t) => t,
        };
        let body = format!(
            "{head} landed on {name} but its release was not activated: {why}\n\nCurrent is untouched, so the landed work is not in force. Last attempt: {retried}.\n\nThe marker {} clears when a release of a later landing activates, or when this check activates {head}'s accepted release; to retry by hand: `release build {head} --repo {}` then `release verify {head}` and `release activate {head}`.\n",
            marker.display(),
            cfg.repo
        );
        let f = Finding::new(db, home_repo, &format!("LAND DEPLOY FAULT: {head} landed but was not released"), &body)
            .priority(1)
            .reference(format!("incident:deploy-fault:{name}:{head}"))
            .cause("deploy-fault");
        if incident::is_usable(incident_sh) {
            incident::alarm(incident_sh, &f);
        }
        log(&format!("watchtower: deploy-fault-check: {name} {head} — {retried}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_head_and_reason_and_rejects_garbage() {
        assert_eq!(parse("abc123 release build exited 1\n"), Some(("abc123", "release build exited 1")));
        assert_eq!(parse("abc123\n"), Some(("abc123", "")));
        assert_eq!(parse("not-a-sha why"), None);
        assert_eq!(parse(""), None);
    }

    /// A git repository with two commits; returns (dir, older, newer).
    fn two_commits(root: &Path) -> (std::path::PathBuf, String, String) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "a"]);
        let a = git(&["rev-parse", "HEAD"]);
        git(&["commit", "-q", "--allow-empty", "-m", "b"]);
        let b = git(&["rev-parse", "HEAD"]);
        (repo, a, b)
    }

    /// sp-6ugk85: a release left accepted and unactivated (its landing's activation was lost)
    /// is activated by the next check, from the release's own binary, and the marker clears.
    /// A retry stamp left by an earlier fault does not hold back a new head's first retry.
    #[test]
    fn an_accepted_release_left_unactivated_is_activated_by_the_next_check() {
        let t = testkit::TempDir::new("deploy-fault-activate");
        let (repo, old, head) = two_commits(t.path());
        let (queue, run_dir, releases) = (t.path().join("queue"), t.path().join("run"), t.path().join("releases"));
        for d in [queue.join("spira"), run_dir.clone(), releases.join(&old), releases.join(&head).join("bin")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let status = t.path().join("release");
        testkit::write_exe(&status, &format!("#!/bin/sh\necho '{{\"current\":\"{old}\",\"releases\":[{{\"sha\":\"{head}\",\"state\":\"accepted\"}}]}}'\n"));
        let calls = t.path().join("calls");
        testkit::write_exe(releases.join(&head).join("bin/release"), &format!("#!/bin/sh\necho \"$*\" >> {}\n", calls.display()));
        std::fs::write(queue.join("spira/deploy-fault"), format!("{head} no release binary at /gone/release\n")).unwrap();
        std::fs::write(run_dir.join("deploy-fault-retry.spira"), format!("{old} 990")).unwrap();
        let cfg = Cfg { release_bin: status.display().to_string(), repo: repo.display().to_string(), retry_secs: 1800, build_timeout_secs: 5, releases: Some(releases.clone()) };
        run(1000, &queue, &run_dir, "db", "spira", "", &cfg);
        let got = std::fs::read_to_string(&calls).unwrap_or_default();
        assert!(got.starts_with(&format!("activate {head} --repo {}", repo.display())), "the check activates the release: {got:?}");
        assert!(!queue.join("spira/deploy-fault").exists(), "a successful activation clears the marker");
    }

    #[test]
    fn a_retry_stamp_counts_only_for_the_head_it_names() {
        assert_eq!(last_attempt("abc 100", "abc"), Some(100));
        assert_eq!(last_attempt("def 100", "abc"), None);
        assert_eq!(last_attempt("100", "abc"), None);
    }

    #[test]
    fn retry_waits_out_the_interval() {
        assert!(retry_due(None, 100, 1800));
        assert!(!retry_due(Some(100), 1899, 1800));
        assert!(retry_due(Some(100), 1900, 1800));
    }
}
