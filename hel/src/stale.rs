//! The worktree reaper: a worktree under `$SPIRA_RUN/worktree` is removed when its bead is
//! finished, its branch is already in the land ref, or nothing has touched it for the idle
//! window. Removal is always `World::destroy_worktree` — the holder witnesses, the salvage and
//! the path fence apply — and a worktree some process has its cwd in is never a candidate.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::git::Git;
use crate::ports::{Repo, World};

pub const DEFAULT_IDLE_SECS: u64 = 48 * 3600;
/// A tree no bead names has no finished state to wait for, so idleness is its only rule.
pub const UNBEADED_IDLE_SECS: u64 = 4 * 3600;
/// A branch with no commits is an ancestor of the land ref the moment it is cut, so the
/// merged rule alone would reap a worktree its dwarf has just been given.
pub const MERGED_GRACE_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub dry: bool,
    pub idle_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    BeadFinished(String),
    BranchMerged(String),
    Idle(u64),
    Keep,
}

/// Pure: the rules in order. `idle` is the worktree's age since its last activity.
pub fn decide(bead_state: Option<&str>, merged_into: Option<&str>, idle: Duration, idle_secs: u64) -> Verdict {
    if let Some(s) = bead_state {
        if matches!(s, "closed" | "LANDED" | "DROPPED") {
            return Verdict::BeadFinished(s.to_string());
        }
    }
    if let Some(r) = merged_into {
        if idle.as_secs() >= MERGED_GRACE_SECS {
            return Verdict::BranchMerged(r.to_string());
        }
    }
    if idle.as_secs() >= idle_secs {
        return Verdict::Idle(idle.as_secs());
    }
    Verdict::Keep
}

/// The newest mtime among the tree's directory and its admin dir's HEAD and index.
pub fn last_activity(w: &Path) -> Option<SystemTime> {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut newest = mtime(w)?;
    if let Ok(dotgit) = std::fs::read_to_string(w.join(".git")) {
        if let Some(admin) = dotgit.trim().strip_prefix("gitdir:") {
            let admin = PathBuf::from(admin.trim());
            for f in ["HEAD", "index"] {
                if let Some(t) = mtime(&admin.join(f)) {
                    newest = newest.max(t);
                }
            }
        }
    }
    Some(newest)
}

/// The first live process whose cwd is `w` or below it.
pub fn live_cwd(proc_root: &Path, w: &Path) -> Option<u32> {
    for e in std::fs::read_dir(proc_root).ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        if let Ok(cwd) = std::fs::read_link(e.path().join("cwd")) {
            if cwd.starts_with(w) {
                return Some(pid);
            }
        }
    }
    None
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub removed: u32,
    pub kept: u32,
    pub failed: u32,
}

/// One worktree as a pass sees it, before any verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeScan {
    pub id: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    /// A live process with its cwd in the tree: it is never a candidate.
    pub live_pid: Option<u32>,
    pub state: Option<String>,
    pub merged_into: Option<String>,
    pub idle_secs: u64,
}

/// One repository's worktrees under the run's worktree directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoScan {
    pub name: String,
    pub root: PathBuf,
    pub landrefs: Vec<String>,
    pub trees: Vec<TreeScan>,
}

/// The reads of a pass, in-process: each configured repository's worktrees under
/// `w.worktrees()` (dot-names skipped), with the live cwd under `proc_root`, the state
/// `state_of` gives, the land ref the branch is merged into, and the idle age.
pub fn scan(w: &dyn World, repos: &[Repo], proc_root: &Path, state_of: &dyn Fn(&str) -> Option<String>) -> Vec<RepoScan> {
    let root_dir = w.worktrees();
    let mut out = vec![];
    for r in repos {
        let Some(root) = r.root.as_deref() else { continue };
        let g = Git(root);
        let landrefs = w.base(root).map(|b| b.landrefs).unwrap_or_default();
        let mut trees = vec![];
        for (path, branch) in g.worktrees() {
            if !path.starts_with(&root_dir) || path == root_dir {
                continue;
            }
            let Some(id) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            if id.starts_with('.') {
                continue;
            }
            let live_pid = live_cwd(proc_root, &path);
            let (state, merged_into, idle_secs) = if live_pid.is_some() {
                (None, None, 0)
            } else {
                let merged = branch.as_deref().and_then(|b| landrefs.iter().find(|lr| g.is_ancestor(b, lr)).cloned());
                let idle = last_activity(&path).and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
                (state_of(&id), merged, idle.as_secs())
            };
            trees.push(TreeScan { id, path, branch, live_pid, state, merged_into, idle_secs });
        }
        out.push(RepoScan { name: r.name.clone(), root: root.to_path_buf(), landrefs, trees });
    }
    out
}

/// A scanned tree's verdict: a live cwd keeps it; a tree with no bead has the shorter
/// window; otherwise [`decide`].
pub fn verdict(state: Option<&str>, merged_into: Option<&str>, idle_secs: u64, live: bool, window_secs: u64) -> Verdict {
    if live {
        return Verdict::Keep;
    }
    let window = if state.is_none() { window_secs.min(UNBEADED_IDLE_SECS) } else { window_secs };
    decide(state, merged_into, Duration::from_secs(idle_secs), window)
}

/// Why a verdict removes a tree, or None for Keep.
pub fn reason(v: &Verdict) -> Option<String> {
    match v {
        Verdict::BeadFinished(s) => Some(format!("bead is {s}")),
        Verdict::BranchMerged(r) => Some(format!("branch is merged into {r}")),
        Verdict::Idle(s) => Some(format!("no activity for {}h", s / 3600)),
        Verdict::Keep => None,
    }
}

pub fn run(w: &dyn World, repos: &[Repo], opts: Opts) -> Tally {
    let mut t = Tally::default();
    w.prefetch();
    let state_of = |id: &str| {
        w.bead(id).map(|b| {
            let s = b.get("status").and_then(|s| s.as_str()).unwrap_or_default().to_string();
            crate::reap::lc_state(id).filter(|l| matches!(l.as_str(), "LANDED" | "DROPPED")).unwrap_or(s)
        })
    };
    for rs in scan(w, repos, Path::new("/proc"), &state_of) {
        for tr in &rs.trees {
            let id = &tr.id;
            if let Some(pid) = tr.live_pid {
                w.emit(&format!("KEEP   {id}  process {pid} has its cwd in {}", tr.path.display()));
                t.kept += 1;
                continue;
            }
            let Some(why) = reason(&verdict(tr.state.as_deref(), tr.merged_into.as_deref(), tr.idle_secs, false, opts.idle_secs)) else {
                t.kept += 1;
                continue;
            };
            if opts.dry {
                w.emit(&format!("WOULD  {id}  {why}"));
                t.removed += 1;
            } else if w.destroy_worktree(id, &tr.path, &rs.root, &format!("reaper: {why}")) {
                w.emit(&format!("REAPED {id}  {why}"));
                t.removed += 1;
            } else {
                w.emit(&format!("FAILED {id}  {why}; not removed, see {}", w.reaplog()));
                t.failed += 1;
            }
        }
        if !opts.dry {
            w.prune(&rs.root);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    #[test]
    fn finished_bead_goes_whatever_its_age() {
        for s in ["closed", "LANDED", "DROPPED"] {
            assert_eq!(decide(Some(s), None, Duration::from_secs(0), 48 * H), Verdict::BeadFinished(s.into()));
        }
    }

    #[test]
    fn open_bead_young_tree_stays() {
        assert_eq!(decide(Some("open"), None, Duration::from_secs(47 * H), 48 * H), Verdict::Keep);
        assert_eq!(decide(None, None, Duration::from_secs(1), 48 * H), Verdict::Keep);
    }

    #[test]
    fn merged_branch_waits_out_the_grace() {
        assert_eq!(decide(Some("in_progress"), Some("main"), Duration::from_secs(60), 48 * H), Verdict::Keep);
        assert_eq!(decide(Some("in_progress"), Some("main"), Duration::from_secs(2 * H), 48 * H), Verdict::BranchMerged("main".into()));
    }

    #[test]
    fn idle_window_is_the_configured_one() {
        assert_eq!(decide(Some("open"), None, Duration::from_secs(48 * H), 48 * H), Verdict::Idle(48 * H));
        assert_eq!(decide(Some("open"), None, Duration::from_secs(5 * H), 4 * H), Verdict::Idle(5 * H));
    }
}
