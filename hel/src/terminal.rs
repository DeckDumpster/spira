//! The reaper for `$SPIRA_RUN/worktree`: a terminal bead's worktree and unclaimed scratch
//! are removed, and nothing else. State comes from the lifecycle machine, never bd status.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::git::Git;
use crate::ports::World;

/// What the worktree's HEAD holds that no ref keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsaved {
    No,
    Yes,
    /// Git could not answer: not a checkout, a land ref that does not resolve.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Terminal,
    Scratch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Remove(Why),
    Keep(&'static str),
}

/// `state`: the lifecycle state of the entry's name, None when no row exists. `is_git`: the
/// entry is a git checkout. `age`: since the newest mtime beneath it. `held`: why a person or
/// process is using it (keep marker, live cwd), which outranks every other reason to reap.
pub fn decide(name: &str, state: Option<&str>, is_git: bool, unsaved: Unsaved, age: Duration, max_age: Duration, held: Option<&'static str>) -> Decision {
    if name.starts_with('.') {
        return Decision::Keep("harness tree");
    }
    if let Some(why) = held {
        return Decision::Keep(why);
    }
    let why = match state {
        Some(s) if spira_config::lc_state::is_terminal(s) => Why::Terminal,
        Some(_) => return Decision::Keep("live bead"),
        None if age < max_age => return Decision::Keep("younger than the scratch age"),
        None => Why::Scratch,
    };
    if is_git {
        match unsaved {
            Unsaved::No => {}
            Unsaved::Yes => return Decision::Keep("commits no ref holds"),
            Unsaved::Unknown => return Decision::Keep("cannot tell what its HEAD holds"),
        }
    }
    Decision::Remove(why)
}

/// HEAD's commits that neither a land ref nor any branch or archive ref reaches. A worktree
/// attached to its own branch holds nothing unsaved: the branch keeps the commits.
pub fn unsaved(wt: &Path, landrefs: &[String]) -> Unsaved {
    let mut args: Vec<&str> = vec!["rev-list", "-n1", "HEAD", "--not"];
    args.extend(landrefs.iter().map(String::as_str));
    args.extend(["--branches", "--glob=refs/archive/*"]);
    match Git(wt).out(&args) {
        Some(s) if s.trim().is_empty() => Unsaved::No,
        Some(_) => Unsaved::Yes,
        None => Unsaved::Unknown,
    }
}

/// The repository a linked worktree belongs to: the parent of its common `.git`.
fn repo_of(wt: &Path) -> Option<PathBuf> {
    let common = Git(wt).git_common_dir()?;
    (common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
}

pub const KEEP_MARKER: &str = ".spira-keep";
const WALK_CAP: usize = 200_000;

fn newest_mtime(p: &Path, budget: &mut usize) -> Option<SystemTime> {
    let m = std::fs::symlink_metadata(p).ok()?;
    let mut newest = m.modified().ok();
    if m.is_dir() {
        for e in std::fs::read_dir(p).ok()?.filter_map(|e| e.ok()) {
            if *budget == 0 {
                return Some(SystemTime::now());
            }
            *budget -= 1;
            newest = newest.max(newest_mtime(&e.path(), budget));
        }
    }
    newest
}

/// Time since the newest mtime anywhere beneath `p`; a tree too large to walk counts as fresh.
fn age_of(p: &Path) -> Duration {
    newest_mtime(p, &mut WALK_CAP.clone())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default()
}

/// A process whose working directory is `p` or inside it.
fn has_live_cwd(p: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(p) else { return false };
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .any(|e| std::fs::read_link(e.path().join("cwd")).is_ok_and(|c| c.starts_with(&real)))
}

fn held_by(p: &Path) -> Option<&'static str> {
    if std::fs::symlink_metadata(p.join(KEEP_MARKER)).is_ok() {
        Some("keep marker")
    } else if has_live_cwd(p) {
        Some("a process has its cwd inside")
    } else {
        None
    }
}

/// One pass. `states`: bead id -> lifecycle state, from one `spira-lc list`. Exit 1 when a
/// removal failed.
pub fn run(w: &dyn World, states: &HashMap<String, String>, max_age: Duration, dry: bool) -> i32 {
    let root = w.worktrees();
    let Ok(rd) = std::fs::read_dir(&root) else {
        w.log(&format!("hel: reap-terminal: cannot read {}", root.display()));
        return 1;
    };
    let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let (mut removed, mut failed) = (0u32, 0u32);
    let mut repos: Vec<PathBuf> = Vec::new();
    for name in names {
        let path = root.join(&name);
        let is_dir = std::fs::symlink_metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        let repo = if is_dir && path.join(".git").exists() { repo_of(&path) } else { None };
        let state = states.get(&name).map(String::as_str);
        let uns = match &repo {
            Some(r) => w.base(r).map_or(Unsaved::Unknown, |b| unsaved(&path, &b.landrefs)),
            None => Unsaved::Unknown,
        };
        let is_git = path.join(".git").exists();
        let decision = decide(&name, state, is_git, if is_git { uns } else { Unsaved::No }, age_of(&path), max_age, held_by(&path));
        match decision {
            Decision::Keep(why) => w.emit(&format!("KEEP    {name}  {why}")),
            Decision::Remove(why) => {
                let tag = if why == Why::Terminal { "terminal" } else { "scratch" };
                if dry {
                    w.emit(&format!("WOULD   remove {name}  {tag}"));
                    continue;
                }
                let ok = match &repo {
                    Some(r) => {
                        let ok = w.destroy_worktree(&name, &path, r, &format!("reap-terminal: {tag}"));
                        if ok && !repos.contains(r) {
                            repos.push(r.clone());
                        }
                        ok
                    }
                    None if is_git => false,
                    None => remove_scratch(&root, &path),
                };
                if ok {
                    removed += 1;
                    w.emit(&format!("REAPED  {name}  {tag}"));
                } else {
                    failed += 1;
                    w.emit(&format!("FAILED  {name}  {tag} removal did not complete"));
                }
            }
        }
    }
    for r in &repos {
        w.prune(r);
    }
    if !dry {
        w.log(&format!("hel: reap-terminal: {removed} removed, {failed} failed"));
    }
    i32::from(failed != 0)
}

/// A non-git entry, removed only when it sits directly under the worktree root; a symlink is
/// unlinked, never followed.
fn remove_scratch(root: &Path, path: &Path) -> bool {
    if path.parent() != Some(root) {
        return false;
    }
    let Ok(m) = std::fs::symlink_metadata(path) else { return true };
    let r = if m.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    r.is_ok() && !path.exists()
}
