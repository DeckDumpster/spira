//! What the clauses read from the world; the real implementation is `real::Real`.

use crate::shell::{DevShell, ShellEnv};
use std::path::Path;

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
    /// The tree's flake dev shell; `Err` when the checker could not find out.
    fn shell_env(&self, repo: &str, tree: Option<&Path>) -> Result<DevShell, String>;
    fn on_path(&self, env: &ShellEnv, program: &str) -> bool;
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
