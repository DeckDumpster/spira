//! Who holds `spira/<id>`: nobody, a finished session's leftover worktree, or a LIVE holder.
//!
//! The defect this module exists for (sp-x1c6k): "checked out in some worktree" was read as
//! "live", and a finished aeon never removes its own worktree, so every submitted bead read as
//! busy forever. Liveness is decided by witnesses, never by the registration alone.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::git::{path_is_under, worktree_of, Git};
use crate::seam::{BeadStatus, Seam};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "holder", rename_all = "kebab-case")]
pub enum Holder {
    /// No worktree holds the branch (or its registered directory is gone).
    None,
    /// A finished session's worktree: no witness, clean, under the sanctioned root.
    Leftover { path: PathBuf },
    /// Somebody is home. Never touched.
    Live { path: PathBuf, witness: String },
    /// A leftover that cannot be touched losslessly.
    Dirty { path: PathBuf, why: String },
    /// Held outside `$SPIRA_RUN/worktree/` — not the harness's to touch.
    Foreign { path: PathBuf },
}

/// The pid recorded in a pidfile, if that process is alive.
fn live_pid(pidfile: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pidfile).ok()?.trim().parse().ok()?;
    Path::new(&format!("/proc/{pid}")).is_dir().then_some(pid)
}

/// Witnesses that somebody is working bead `id`; Some(why) when any says so.
pub fn witness(run: &Path, id: &str, seam: &dyn Seam) -> Option<String> {
    // 1. an operator/tool hold (hold.sh): any live pid.
    if let Some(pid) = live_pid(&run.join(format!("hold-{id}.pid"))) {
        return Some(format!("hold-{id}.pid names live pid {pid}"));
    }
    // 2. the bead's aeon: aeon-<fayth>-<id>.pid whose identity lease is still running.
    if let Ok(rd) = std::fs::read_dir(run) {
        let suffix = format!("-{id}.pid");
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("aeon-") && n.ends_with(&suffix) && sending::reap::aeon_alive(&e.path()) {
                return Some(format!("{n} holds a running identity lease"));
            }
        }
    }
    // 3/4. the lease: a WORKING row, or a machine that cannot prove otherwise.
    match seam.bead_status(id) {
        BeadStatus::Unreachable => {
            Some("the lifecycle machine did not answer, so the claim witness proves nothing".into())
        }
        BeadStatus::Known(s) if spira_config::lc_state::is_working(&s) => {
            Some("WORKING — the lease has not been released".into())
        }
        BeadStatus::Known(_) => None,
    }
}

/// A process whose cwd is inside `dir` (a hand session, an editor, a running tool).
pub fn cwd_witness(dir: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).ok()?;
    let me = std::process::id();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        if let Ok(cwd) = std::fs::read_link(e.path().join("cwd")) {
            if cwd.starts_with(&dir) {
                return Some(format!("pid {pid} is working in {}", dir.display()));
            }
        }
    }
    None
}

/// Classifies the holder of `refs/heads/<branch>` for bead `id`.
pub fn classify(repo: &Git, branch: &str, id: &str, run: &Path, seam: &dyn Seam) -> Holder {
    let Some(path) = worktree_of(repo, branch) else {
        return Holder::None;
    };
    if !path.exists() {
        // A dangling registration: nothing on disk to protect, and every operation here is on
        // a detached scratch tree plus a ref update, which a dangling entry does not block.
        return Holder::None;
    }
    let sanctioned = run.join("worktree");
    if !path_is_under(&path, &sanctioned) {
        return Holder::Foreign { path };
    }
    if let Some(w) = witness(run, id, seam) {
        return Holder::Live { path, witness: w };
    }
    // sp-87csm: a child bead's directory can hold its parent's branch — the path's own bead
    // is asked too.
    if let Some(pid_name) = path.file_name().and_then(|n| n.to_str()).map(String::from) {
        if pid_name != id {
            if let Some(w) = witness(run, &pid_name, seam) {
                return Holder::Live {
                    path,
                    witness: format!("{w} (as {pid_name})"),
                };
            }
        }
    }
    if let Some(w) = cwd_witness(&path) {
        return Holder::Live { path, witness: w };
    }
    let wt = Git::new(&path, &repo.name, &repo.email);
    if wt.mid_rebase() || wt.mid_merge() {
        return Holder::Dirty {
            path,
            why: "a rebase or merge is in progress there".into(),
        };
    }
    match wt.porcelain() {
        None => Holder::Dirty {
            path,
            why: "its status cannot be read".into(),
        },
        Some(s) if !s.trim().is_empty() => Holder::Dirty {
            path,
            why: "it has uncommitted changes".into(),
        },
        Some(_) => Holder::Leftover { path },
    }
}
