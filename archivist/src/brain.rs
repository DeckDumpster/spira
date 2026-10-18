//! Commits the wiki pages an archive run wrote, by explicit path, before the run reports safe.
//!
//! A path is the run's own only if it was clean or differently-dirty before the agent started
//! and dirty now: whatever was already dirty belongs to someone else and is never staged.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub type Snapshot = BTreeMap<String, String>;

const LOCAL_DEADLINE_SECS: &str = "5";
const NETWORK_DEADLINE_SECS: &str = "60";

fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let network = matches!(args.first(), Some(&"pull") | Some(&"push"));
    let mut c = Command::new("timeout");
    c.arg(if network { NETWORK_DEADLINE_SECS } else { LOCAL_DEADLINE_SECS })
        .args(["git", "-C"])
        .arg(dir)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) {
        let _ = si.write_all(text.as_bytes());
    }
    let o = child.wait_with_output().map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()))
    }
}

pub fn is_ours_to_commit(path: &str) -> bool {
    path.starts_with("wiki/") || path == "index.md" || path == "log.md"
}

/// Every dirty path under the commit bounds, with a fingerprint of its content (`-` when deleted).
pub fn snapshot(dir: &Path) -> Result<Snapshot, String> {
    let out = git(dir, &["status", "--porcelain", "-z", "--untracked-files=all"], None)?;
    let mut snap = Snapshot::new();
    let mut parts = out.split('\0').filter(|p| !p.is_empty());
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = entry.split_at(3);
        if xy.starts_with('R') || xy.starts_with('C') {
            parts.next();
        }
        if !is_ours_to_commit(path) {
            continue;
        }
        let print = if dir.join(path).is_file() { git(dir, &["hash-object", "--", path], None)?.trim().to_string() } else { "-".to_string() };
        snap.insert(path.to_string(), print);
    }
    Ok(snap)
}

pub fn touched(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    after.iter().filter(|(p, h)| before.get(*p) != Some(*h)).map(|(p, _)| p.clone()).collect()
}

/// Stage exactly `paths`, commit, rebase onto the remote, push. Returns the count committed.
pub fn commit_paths(dir: &Path, paths: &[String], subject: &str) -> Result<usize, String> {
    if paths.is_empty() {
        return Ok(0);
    }
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    git(dir, &add, None)?;
    let mut commit: Vec<&str> = vec!["commit", "-F", "-", "--only", "--"];
    commit.extend(paths.iter().map(String::as_str));
    git(dir, &commit, Some(subject))?;
    git(dir, &["pull", "--rebase"], None)?;
    git(dir, &["push"], None)?;
    Ok(paths.len())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sh(dir: &Path, args: &[&str]) {
        let st = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
        assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
    }

    pub(crate) fn fixture() -> (testkit::TempDir, std::path::PathBuf) {
        let root = testkit::TempDir::new("archivist-brain");
        let remote = root.join("remote.git");
        let brain = root.join("brain");
        sh(root.path(), &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
        sh(root.path(), &["clone", "-q", remote.to_str().unwrap(), brain.to_str().unwrap()]);
        sh(&brain, &["config", "user.name", "t"]);
        sh(&brain, &["config", "user.email", "t@t"]);
        std::fs::create_dir_all(brain.join("wiki/notes")).unwrap();
        std::fs::write(brain.join("wiki/notes/seed.md"), "seed\n").unwrap();
        std::fs::write(brain.join("index.md"), "idx\n").unwrap();
        sh(&brain, &["add", "-A"]);
        sh(&brain, &["commit", "-q", "-m", "seed"]);
        sh(&brain, &["push", "-q", "-u", "origin", "main"]);
        (root, brain)
    }

    fn porcelain(dir: &Path) -> String {
        git(dir, &["status", "--porcelain", "--untracked-files=all"], None).unwrap()
    }

    #[test]
    fn a_run_touching_two_pages_leaves_them_committed_and_the_tree_clean() {
        let (_root, brain) = fixture();
        let before = snapshot(&brain).unwrap();
        std::fs::write(brain.join("wiki/notes/a.md"), "a\n").unwrap();
        std::fs::write(brain.join("wiki/notes/seed.md"), "seed edited\n").unwrap();
        let after = snapshot(&brain).unwrap();
        let paths = touched(&before, &after);
        assert_eq!(paths, vec!["wiki/notes/a.md".to_string(), "wiki/notes/seed.md".to_string()]);
        assert_eq!(commit_paths(&brain, &paths, "archivist: 2 page(s) from session s1\n").unwrap(), 2);
        assert_eq!(porcelain(&brain), "");
        let remote_head = git(&brain, &["rev-parse", "origin/main"], None).unwrap();
        assert_eq!(remote_head, git(&brain, &["rev-parse", "HEAD"], None).unwrap());
    }

    #[test]
    fn a_path_dirty_before_the_run_and_untouched_by_it_is_not_committed() {
        let (_root, brain) = fixture();
        std::fs::write(brain.join("wiki/notes/other.md"), "someone else\n").unwrap();
        let before = snapshot(&brain).unwrap();
        std::fs::write(brain.join("wiki/notes/mine.md"), "mine\n").unwrap();
        let paths = touched(&before, &snapshot(&brain).unwrap());
        assert_eq!(paths, vec!["wiki/notes/mine.md".to_string()]);
        commit_paths(&brain, &paths, "archivist: 1 page(s)\n").unwrap();
        assert_eq!(porcelain(&brain), "?? wiki/notes/other.md\n");
    }

    #[test]
    fn a_path_dirty_before_and_edited_again_by_the_run_is_committed() {
        let (_root, brain) = fixture();
        std::fs::write(brain.join("wiki/notes/p.md"), "one\n").unwrap();
        let before = snapshot(&brain).unwrap();
        std::fs::write(brain.join("wiki/notes/p.md"), "two\n").unwrap();
        assert_eq!(touched(&before, &snapshot(&brain).unwrap()), vec!["wiki/notes/p.md".to_string()]);
    }

    #[test]
    fn paths_outside_the_wiki_bounds_are_never_candidates() {
        let (_root, brain) = fixture();
        let before = snapshot(&brain).unwrap();
        std::fs::write(brain.join("stray.txt"), "x\n").unwrap();
        std::fs::create_dir_all(brain.join(".claude")).unwrap();
        std::fs::write(brain.join(".claude/s.json"), "{}\n").unwrap();
        std::fs::write(brain.join("log.md"), "entry\n").unwrap();
        assert_eq!(touched(&before, &snapshot(&brain).unwrap()), vec!["log.md".to_string()]);
    }

    #[test]
    fn a_failed_push_is_an_error_not_a_silent_success() {
        let (_root, brain) = fixture();
        sh(&brain, &["remote", "set-url", "origin", "/nonexistent/remote.git"]);
        std::fs::write(brain.join("wiki/notes/a.md"), "a\n").unwrap();
        let err = commit_paths(&brain, &["wiki/notes/a.md".to_string()], "archivist: x\n").unwrap_err();
        assert!(err.contains("git pull --rebase failed") || err.contains("git push failed"), "{err}");
    }

    #[test]
    fn nothing_touched_commits_nothing() {
        let (_root, brain) = fixture();
        assert_eq!(commit_paths(&brain, &[], "x\n").unwrap(), 0);
    }
}
