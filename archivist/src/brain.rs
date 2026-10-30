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
        if repaired { ok.push(p.clone()) } else { refused.push(p.clone()) }
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

fn refusal(refused: &[String]) -> String {
    format!("not committed, they violate the frontmatter schema: {}", refused.join(", "))
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
    fn a_dirty_tracked_file_in_the_tree_does_not_stop_the_pages_being_pushed() {
        let (_root, brain) = fixture();
        std::fs::write(brain.join("wiki/notes/seed.md"), "edited by someone else\n").unwrap();
        std::fs::write(brain.join("wiki/notes/mine.md"), "mine\n").unwrap();
        commit_paths(&brain, &["wiki/notes/mine.md".to_string()], "archivist: 1 page(s)\n").unwrap();
        assert_eq!(porcelain(&brain), " M wiki/notes/seed.md\n");
        assert_eq!(git(&brain, &["rev-parse", "origin/main"], None).unwrap(), git(&brain, &["rev-parse", "HEAD"], None).unwrap());
    }

    #[test]
    fn a_failed_push_is_an_error_not_a_silent_success() {
        let (_root, brain) = fixture();
        sh(&brain, &["remote", "set-url", "origin", "/nonexistent/remote.git"]);
        std::fs::write(brain.join("wiki/notes/a.md"), "a\n").unwrap();
        let err = commit_paths(&brain, &["wiki/notes/a.md".to_string()], "archivist: x\n").unwrap_err();
        assert!(err.contains("git pull --rebase --autostash failed") || err.contains("git push failed"), "{err}");
    }

    fn stub_git(root: &Path) -> (String, std::path::PathBuf) {
        let count = root.join("commit-count");
        let stub = root.join("git-stub");
        testkit::write_exe(
            &stub,
            &format!("#!/bin/sh\ncase \" $* \" in *\" commit \"*) echo x >> {}; exit 124;; esac\nexec git \"$@\"\n", count.display()),
        );
        (stub.to_str().unwrap().to_string(), count)
    }

    #[test]
    fn a_commit_that_times_out_is_retried_named_and_leaves_nothing_staged() {
        let (root, brain) = fixture();
        let (program, count) = stub_git(root.path());
        let g = Git { program, local_deadline: "7".to_string() };
        std::fs::write(brain.join("wiki/notes/a.md"), "a\n").unwrap();
        let err = commit_paths_with(&g, &brain, &["wiki/notes/a.md".to_string()], "archivist: x\n").unwrap_err();
        assert!(err.contains("deadline 7s exceeded"), "{err}");
        assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 2);
        assert_eq!(porcelain(&brain), "?? wiki/notes/a.md\n");
    }

    #[test]
    fn a_failure_other_than_the_deadline_names_its_exit_status() {
        let (_root, brain) = fixture();
        let err = git(&brain, &["rev-parse", "--verify", "nosuchref"], None).unwrap_err();
        assert!(err.contains("exit 128"), "{err}");
    }

    const STUB_VALIDATOR: &str = "import sys,re\nt=open(sys.argv[1]).read()\nbad=[l for l in t.split('\\n') if re.match(r'^\\s+- ',l) and len(l.strip()[2:].strip('\"\\''))>60]\nn=sum(1 for l in t.split('\\n') if re.match(r'^\\s+- ',l))\nsys.exit(1 if bad or n>10 else 0)\n";

    fn with_validator(brain: &Path) {
        std::fs::create_dir_all(brain.join(".claude/skills/wiki-lint")).unwrap();
        std::fs::write(brain.join(VALIDATOR), STUB_VALIDATOR).unwrap();
        sh(brain, &["add", "-A"]);
        sh(brain, &["commit", "-q", "-m", "validator"]);
    }

    fn page(alias: &str) -> String {
        format!("---\ntype: note\naliases:\n  - short one\n  - {alias}\n---\n\n# x\n")
    }

    #[test]
    fn a_68_char_alias_is_dropped_before_the_commit() {
        let (_root, brain) = fixture();
        with_validator(&brain);
        let long = "a".repeat(68);
        std::fs::write(brain.join("wiki/notes/a.md"), page(&long)).unwrap();
        assert_eq!(commit_paths(&brain, &["wiki/notes/a.md".to_string()], "archivist: x\n").unwrap(), 1);
        let committed = git(&brain, &["show", "HEAD:wiki/notes/a.md"], None).unwrap();
        assert!(!committed.contains(&long) && committed.contains("short one"), "{committed}");
    }

    fn crowded(extra: usize) -> String {
        let old: String = (0..9).map(|i| format!("  - syn{i}\n")).collect();
        let new: String = (0..extra).map(|i| format!("  - keyword{i}\n")).collect();
        format!("---\ntype: note\naliases:\n{old}{new}---\n\n# x\n")
    }

    #[test]
    fn aliases_added_past_ten_are_dropped_and_the_existing_synonyms_kept() {
        let (_root, brain) = fixture();
        with_validator(&brain);
        std::fs::write(brain.join("wiki/notes/a.md"), crowded(0)).unwrap();
        commit_paths(&brain, &["wiki/notes/a.md".to_string()], "archivist: x\n").unwrap();
        std::fs::write(brain.join("wiki/notes/a.md"), crowded(5)).unwrap();
        assert_eq!(commit_paths(&brain, &["wiki/notes/a.md".to_string()], "archivist: y\n").unwrap(), 1);
        let committed = git(&brain, &["show", "HEAD:wiki/notes/a.md"], None).unwrap();
        assert_eq!(committed.matches("\n  - ").count(), 10, "{committed}");
        assert!(committed.contains("syn8") && committed.contains("keyword0") && !committed.contains("keyword1"), "{committed}");
        assert_eq!(porcelain(&brain), "");
    }

    #[test]
    fn a_page_the_validator_rejects_for_another_reason_is_refused_not_committed() {
        let (_root, brain) = fixture();
        with_validator(&brain);
        std::fs::write(brain.join(VALIDATOR), "import sys\nsys.exit(1)\n").unwrap();
        sh(&brain, &["commit", "-qam", "strict"]);
        let head = git(&brain, &["rev-parse", "HEAD"], None).unwrap();
        std::fs::write(brain.join("wiki/notes/a.md"), "no frontmatter\n").unwrap();
        let err = commit_paths(&brain, &["wiki/notes/a.md".to_string()], "archivist: x\n").unwrap_err();
        assert!(err.contains("wiki/notes/a.md"), "{err}");
        assert_eq!(head, git(&brain, &["rev-parse", "HEAD"], None).unwrap());
    }

    #[test]
    fn nothing_touched_commits_nothing() {
        let (_root, brain) = fixture();
        assert_eq!(commit_paths(&brain, &[], "x\n").unwrap(), 0);
    }
}
