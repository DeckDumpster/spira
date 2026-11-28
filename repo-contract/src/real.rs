//! The ports over git, nix and the repo registry.

use crate::ports::{Piece, Ports};
use crate::shell::{self, ShellEnv};
use spira_config::repo_admin::cut_worktree;
use spira_config::repos::{self, Registry};
use std::path::{Path, PathBuf};

pub struct Real {
    reg: Registry,
    cached_shell: bool,
}

impl Real {
    pub fn from_env() -> Result<Real, String> {
        let home = spira_config::resolve::locate_home_for_process()?;
        Ok(Real { reg: Registry::from_env_checkout(std::env::vars().collect(), &home), cached_shell: false })
    }

    /// With `cached_shell`, a landing-ref dev shell that is not already realised is not
    /// realised here: no worktree is cut and nix does not run.
    pub fn cached_shell(self, cached_shell: bool) -> Real {
        Real { cached_shell, ..self }
    }

    pub fn root(&self, repo: &str) -> Result<PathBuf, String> {
        self.reg.root(repo).map(PathBuf::from).ok_or_else(|| format!("{repo} is not in the repo registry"))
    }

    pub fn land(&self, repo: &str) -> Result<String, String> {
        repos::landref(&self.reg, repo).ok_or_else(|| format!("{repo}'s landing ref does not resolve"))
    }

    fn git(&self, repo: &str, args: &[&str]) -> Result<std::process::Output, String> {
        spira_config::bounded::bounded("git").args(args).current_dir(self.root(repo)?).output().map_err(|e| format!("git: {e}"))
    }

    fn in_tree<T>(&self, repo: &str, tree: Option<&Path>, f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
        match tree {
            Some(t) => f(t),
            None => {
                let wt = cut_worktree(&self.root(repo)?, &self.land(repo)?)?;
                f(&wt.path())
            }
        }
    }
}

impl Ports for Real {
    fn read(&self, repo: &str, tree: Option<&Path>, rel: &str) -> Result<Option<String>, String> {
        if let Some(t) = tree {
            return match std::fs::read_to_string(t.join(rel)) {
                Ok(s) => Ok(Some(s)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("{}: {e}", t.join(rel).display())),
            };
        }
        let spec = format!("{}:{rel}", self.land(repo)?);
        if !self.git(repo, &["cat-file", "-e", &spec])?.status.success() {
            return Ok(None);
        }
        let out = self.git(repo, &["show", &spec])?;
        if !out.status.success() {
            return Err(format!("git show {spec}: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    fn shell_env(&self, repo: &str, tree: Option<&Path>) -> Result<shell::DevShell, String> {
        let cache = shell::cache_dir()?;
        if tree.is_none() {
            let Some(flake_nix) = self.read(repo, None, "flake.nix")? else { return Ok(shell::DevShell::Undeclared) };
            if !shell::declares_dev_shell(flake_nix.as_bytes()) {
                return Ok(shell::DevShell::Undeclared);
            }
            let flake_lock = self.read(repo, None, "flake.lock")?.unwrap_or_default();
            let key = shell::cache_key(flake_nix.as_bytes(), flake_lock.as_bytes());
            if let Some(env) = shell::cached_env(&cache, &key) {
                return Ok(shell::DevShell::Entered(env));
            }
            if self.cached_shell {
                return Err(format!("the dev shell for flake {key} is not realised; repo-contract check {repo} realises it"));
            }
        }
        let nix = shell::nix_bin()?;
        self.in_tree(repo, tree, |t| shell::shell_env(t, &nix, &cache))
    }

    fn on_path(&self, env: &ShellEnv, program: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        env.path_dirs().iter().any(|d| d.join(program).metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    }

    fn landing_pieces(&self, repo: &str) -> Vec<Piece> {
        spira_config::repo_admin::check(&self.reg, repo).into_iter().map(|p| Piece { name: p.name.into(), detail: p.detail, ok: p.ok }).collect()
    }

    fn land_mode(&self, repo: &str) -> String {
        self.reg.land(repo)
    }

    fn workflow(&self, repo: &str) -> Result<String, String> {
        let default = spira_config::process::cfg("SPIRA_FORGE_WORKFLOW")?;
        let table = spira_config::process::cfg("SPIRA_FORGE_WORKFLOWS")?;
        Ok(forge::cmds::workflow_for(&default, &table, Some(repo)).to_string())
    }

    fn workflow_files(&self, repo: &str) -> Result<Vec<String>, String> {
        let spec = format!("{}:{}/", self.land(repo)?, crate::clause::WORKFLOWS);
        let out = self.git(repo, &["ls-tree", "--name-only", &spec])?;
        if !out.status.success() {
            return Err(format!("git ls-tree {spec}: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).lines().map(|l| l.rsplit('/').next().unwrap_or(l).to_string()).collect())
    }

    fn product_repos(&self) -> Vec<String> {
        crate::clause::without_home(self.reg.all(), self.reg.home_repo())
    }

    fn registered(&self, repo: &str) -> bool {
        self.reg.names().iter().any(|n| n == repo)
    }
}
