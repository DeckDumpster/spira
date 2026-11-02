//! Commits the wiki pages an archive run wrote, by explicit path, before the run reports safe.
//!
//! A path is the run's own only if it was clean or differently-dirty before the agent started
//! and dirty now: whatever was already dirty belongs to someone else and is never staged.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub type Snapshot = BTreeMap<String, String>;

const LOCAL_DEADLINE_SECS: &str = "30";
const NETWORK_DEADLINE_SECS: &str = "60";
const TIMEOUT_EXIT: i32 = 124;

struct Git {
    program: String,
    local_deadline: String,
}

impl Git {
    fn real() -> Git {
        #[cfg(test)]
        if let Some(program) = tests::GIT_PROGRAM.with(|p| p.borrow().clone()) {
            return Git { program, local_deadline: LOCAL_DEADLINE_SECS.to_string() };
        }
        Git { program: "git".to_string(), local_deadline: LOCAL_DEADLINE_SECS.to_string() }
    }
}

fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    run_git(&Git::real(), dir, args, stdin)
}

fn run_git(g: &Git, dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let network = matches!(args.first(), Some(&"pull") | Some(&"push"));
    let deadline = if network { NETWORK_DEADLINE_SECS } else { g.local_deadline.as_str() };
    let mut c = Command::new("timeout");
    c.arg(deadline)
        .arg(&g.program)
        .arg("-C")
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
        let stderr = String::from_utf8_lossy(&o.stderr);
        let reason = match o.status.code() {
            Some(TIMEOUT_EXIT) => format!("deadline {deadline}s exceeded"),
            Some(code) => format!("exit {code}: {}", stderr.trim()),
            None => format!("killed by a signal: {}", stderr.trim()),
        };
        Err(format!("git {} failed: {reason}", args.join(" ")))
    }
}

fn timed_out(err: &str) -> bool {
    err.contains("deadline ") && err.contains("s exceeded")
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

const VALIDATOR: &str = ".claude/skills/wiki-lint/frontmatter.py";
const ALIAS_MAX: usize = 60;
const ALIAS_COUNT_MAX: usize = 10;

fn passes_schema(dir: &Path, path: &str) -> bool {
    Command::new("timeout")
        .arg(NETWORK_DEADLINE_SECS)
        .args(["python3", VALIDATOR, path])
        .current_dir(dir)
        .env("BRAIN", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn alias_len(item: &str) -> usize {
    item.trim().trim_matches(|c| c == '"' || c == '\'').chars().count()
}

fn alias_item(line: &str) -> Option<&str> {
    let bare = line.trim_end_matches('\n');
    bare.trim_start().strip_prefix("- ").filter(|_| bare.starts_with([' ', '\t'])).map(str::trim)
}

/// Block-list `aliases:` items of a page's frontmatter, with the line each sits on.
fn alias_lines(text: &str) -> (Option<usize>, Vec<(usize, String)>) {
    let (mut in_front, mut in_aliases) = (false, false);
    let (mut key, mut items) = (None, Vec::new());
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let bare = line.trim_end_matches('\n');
        if n == 0 && bare == "---" {
            in_front = true;
        } else if in_front && bare == "---" {
            break;
        } else if in_front && bare == "aliases:" {
            (in_aliases, key) = (true, Some(n));
        } else if in_front && in_aliases {
            match alias_item(line) {
                Some(item) => items.push((n, item.to_string())),
                None => in_aliases = false,
            }
        }
    }
    (key, items)
}

/// Drops block-list `aliases:` items longer than the schema allows, then the newest beyond
/// `ALIAS_COUNT_MAX` (those absent from `head`, last first, then the last of what remains);
/// the key goes too if none remain.
fn trim_aliases(text: &str, head: &str) -> String {
    let (key, items) = alias_lines(text);
    let (_, old) = alias_lines(head);
    let mut kept: Vec<&(usize, String)> = items.iter().filter(|(_, i)| alias_len(i) <= ALIAS_MAX).collect();
    while kept.len() > ALIAS_COUNT_MAX {
        let at = kept.iter().rposition(|(_, i)| !old.iter().any(|(_, o)| o == i)).unwrap_or(kept.len() - 1);
        kept.remove(at);
    }
    let mut dropped: Vec<usize> = items.iter().map(|(n, _)| *n).filter(|n| !kept.iter().any(|(k, _)| k == n)).collect();
    if kept.is_empty() {
        dropped.extend(key);
    }
    text.split_inclusive('\n').enumerate().filter(|(n, _)| !dropped.contains(n)).map(|(_, l)| l).collect()
}

/// Splits `paths` into those the brain's pre-receive hook will accept (fixing an over-long alias
/// mechanically where that is all that is wrong) and those it will reject. A brain without the
/// validator is not vetted here; the hook is then the only judge.
fn vet(dir: &Path, paths: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut ok, mut refused) = (Vec::new(), Vec::new());
    for p in paths {
        let checked = p.starts_with("wiki/") && p.ends_with(".md") && dir.join(p).is_file() && dir.join(VALIDATOR).is_file();
        if !checked || passes_schema(dir, p) {
            ok.push(p.clone());
            continue;
        }
        let head = git(dir, &["show", &format!("HEAD:{p}")], None).unwrap_or_default();
        let fixed = std::fs::read_to_string(dir.join(p)).map(|t| trim_aliases(&t, &head));
        let repaired = match fixed {
            Ok(t) => std::fs::write(dir.join(p), t).is_ok() && passes_schema(dir, p),
            Err(_) => false,
        };
        if repaired {
            eprintln!("archivist: trimmed aliases on {p} to fit the frontmatter schema");
            ok.push(p.clone())
        } else { refused.push(p.clone()) }
    }
    (ok, refused)
}

/// Stage exactly `paths`, commit, rebase onto the remote, push. Returns the count committed.
pub fn commit_paths(dir: &Path, paths: &[String], subject: &str) -> Result<usize, String> {
    commit_paths_with(&Git::real(), dir, paths, subject)
}

/// A failed stage or commit unstages `paths`, so a later plain `git commit` cannot sweep them in.
fn commit_paths_with(g: &Git, dir: &Path, paths: &[String], subject: &str) -> Result<usize, String> {
    let (vetted, refused) = vet(dir, paths);
    let paths = &vetted[..];
    if paths.is_empty() {
        return if refused.is_empty() { Ok(0) } else { Err(refusal(&refused)) };
    }
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    let mut commit: Vec<&str> = vec!["commit", "-F", "-", "--only", "--"];
    commit.extend(paths.iter().map(String::as_str));
    let staged = run_git(g, dir, &add, None).and_then(|_| match run_git(g, dir, &commit, Some(subject)) {
        Err(e) if timed_out(&e) => run_git(g, dir, &commit, Some(subject)),
        r => r,
    });
    if let Err(e) = staged {
        let mut reset: Vec<&str> = vec!["reset", "-q", "--"];
        reset.extend(paths.iter().map(String::as_str));
        let _ = run_git(g, dir, &reset, None);
        return Err(e);
    }
    run_git(g, dir, &["pull", "--rebase", "--autostash"], None)?;
    run_git(g, dir, &["push"], None)?;
    if refused.is_empty() { Ok(paths.len()) } else { Err(refusal(&refused)) }
}

/// A `pull --rebase` refused because the checkout holds another writer's uncommitted
/// changes. Transient: retried later, and the changes are never stashed or discarded here.
pub fn is_dirty_checkout_refusal(err: &str) -> bool {
    err.contains("cannot pull with rebase") || err.contains("You have unstaged changes") || err.contains("Your local changes would be overwritten")
}

fn refusal(refused: &[String]) -> String {
    format!("not committed, they violate the frontmatter schema: {}", refused.join(", "))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    thread_local! {
        /// A stand-in for `git` that only this thread's `Git::real()` uses.
        pub(crate) static GIT_PROGRAM: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }

    #[test]
    fn the_dirty_checkout_refusal_is_recognised_and_other_failures_are_not() {
        assert!(is_dirty_checkout_refusal("git pull --rebase failed: exit 128: error: cannot pull with rebase: You have unstaged changes."));
        assert!(!is_dirty_checkout_refusal("could not read from remote repository"));
    }
}
