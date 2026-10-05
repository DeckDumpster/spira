//! What the sweep asks of the world (DESIGN.md §3): lib.sh's chokepoints, bd, the forge and
//! the lifecycle machine. git is not here — the sweep reads refs itself (git.rs), because
//! every git question it asks is local and read-only, and a test drives it against a real
//! repository rather than a model of one.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// One repository the harness manages (read once, from the context seam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    /// None: no path is configured for it.
    pub root: Option<PathBuf>,
    /// Land mode queue or queue.local (repo_land_queued).
    pub queued: bool,
}

/// A repository's land ref, its land refs (the base plus any local landing ref), and the
/// base's own remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    pub landref: String,
    pub landrefs: Vec<String>,
    pub remote: Option<String>,
}

/// The outcome of `send`: the mid-send recheck, then spira_reap_landed_branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// Somebody may be home (the witness's reason).
    Held(String),
    /// Worktree, branch, remote branch and branch: label are gone.
    Done,
    /// The branch is CERTIFIED/BATCHED — waiting for its verdict.
    Queued,
    /// Refused or failed; SPIRA_REAP_ERR.
    Failed(String),
}

pub trait World {
    /// spira_landref and friends for one checkout; None when the land ref does not resolve.
    fn base(&self, root: &Path) -> Option<Base>;
    /// Read every bead's record once, so `bead` and `witness` answer from it for the rest of
    /// the pass. Mutations still recheck live (`send`).
    fn prefetch(&self) {}
    /// spira_holder_witnesses: Some(why) when somebody may be home.
    fn witness(&self, id: &str) -> Option<String>;
    /// The bead's bd record (`bdjson show`'s first row); None when bd has none (or failed).
    fn bead(&self, id: &str) -> Option<Value>;
    /// The mid-send recheck and the verified deletion (spira_reap_landed_branch).
    fn send(&self, id: &str, br: &str, repo: &Path, why: &str, caller: &str) -> Sent;
    /// `spira-lc close-on-land`.
    fn close_on_land(&self, id: &str, sha: &str);
    /// spira_destroy_worktree; true when removed (or nothing to remove).
    fn destroy_worktree(&self, id: &str, w: &Path, repo: &Path, why: &str) -> bool;
    /// spira_prune_worktrees.
    fn prune(&self, repo: &Path);
    /// `spira-lc state <id>` reads LANDED: the lifecycle record says the bead landed. An
    /// unreadable record or a missing row is not LANDED (cannot prove it landed).
    fn lc_landed(&self, id: &str) -> bool;
    /// `spira-lc content-on-base <id> <proof> sending`.
    fn content_on_base(&self, id: &str, proof: &str);
    /// The merged PR's head for `br`, if a PR for it is MERGED (`gh pr view`).
    fn pr_merged_tip(&self, repo: &Path, br: &str) -> Option<String>;
    /// One line of our own output, in order.
    fn emit(&self, line: &str);
    /// lib.sh `log`: one timestamped line of our own output.
    fn log(&self, msg: &str);
    /// The reap log, for FAILED lines that name it.
    fn reaplog(&self) -> String;
    /// $SPIRA_RUN/worktree — the only directory whose orphans PASS 2 judges.
    fn worktrees(&self) -> PathBuf;
}
