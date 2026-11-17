//! What the clauses read from the world; the real implementation is `real::Real`.

use crate::shell::ShellEnv;
use std::path::Path;

/// The verdict of the test entry point's run of a tier list, which reports one verdict for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier {
    pub name: String,
    pub ok: bool,
}

/// `work checks` in a tree: whether it passed and each row's `-- <name>:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows {
    pub ok: bool,
    pub lines: Vec<String>,
}

/// A registration piece, as `spira-config repo check` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub name: String,
    pub detail: String,
    pub ok: bool,
}

/// Every method that can fail for a reason other than the repository's contract returns `Err`:
/// the check then cannot tell, which is not the same as not met.
pub trait Ports {
    /// A file of `tree`, or of the repository's landing ref when `tree` is `None`; `None` when absent.
    fn read(&self, repo: &str, tree: Option<&Path>, rel: &str) -> Result<Option<String>, String>;
    /// The tree's flake dev shell; `Ok(None)` when it declares none.
    fn shell_env(&self, repo: &str, tree: Option<&Path>) -> Result<Option<ShellEnv>, String>;
    fn on_path(&self, env: &ShellEnv, program: &str) -> bool;
    /// `work checks` in a detached worktree cut from the landing ref, or in `tree`.
    fn run_checks(&self, repo: &str, tree: Option<&Path>) -> Result<Rows, String>;
    /// `testenv --suites <csv> <land ref> <repo>`.
    fn run_tiers(&self, repo: &str, suites: &[String]) -> Result<Vec<Tier>, String>;
    fn landing_pieces(&self, repo: &str) -> Vec<Piece>;
    fn land_mode(&self, repo: &str) -> String;
    /// The CI workflow's name for the repository, as `forge` resolves it.
    fn workflow(&self, repo: &str) -> Result<String, String>;
    /// File names under `.github/workflows/` on the landing ref.
    fn workflow_files(&self, repo: &str) -> Result<Vec<String>, String>;
    /// Every registered repository but the home repo.
    fn product_repos(&self) -> Vec<String>;
    fn registered(&self, repo: &str) -> bool;
}
